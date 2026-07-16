# `std::database::sql`

Status: experimental

Driver-pluggable SQL database access. No driver ships in the box; bring your own (Postgres, MySQL, SQLite, ...) by registering one at startup.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Conn`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `type Conn` | Open database connection. `prepare`, `execute`, `query`, `query_each`, `begin`, `begin_with`, `ping`, `execute_many`, `execute_ctx`, `query_ctx`, `interrupt`, `close` (closing sweeps any cursors still open on the connection). |
| [`Driver`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `trait Driver` | Host-side database driver contract. Rust bindings register implementations before the Gossamer program starts. |
| [`Error`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `type Error` | Driver error. `Error::driver(driver, msg)` builds one; `Error::PoolExhausted` and `Error::Cancelled` are variants. |
| [`IsolationLevel`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `type IsolationLevel` | Default / ReadUncommitted / ReadCommitted / RepeatableRead / Serializable. Passed to `Conn::begin_with`. |
| [`Pool`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `type Pool` | Connection pool. `new`, `fill`, `get` (blocks up to `acquire_timeout`), `len`. Cheap to clone. |
| [`PoolConfig`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `type PoolConfig` | Pool tuning: `min`, `max`, `idle_timeout`, `max_lifetime`, `acquire_timeout`, `statement_cache`. Fluent `with_*` builders. |
| [`PooledConn`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `type PooledConn` | Connection checked out from a `Pool`; returned on drop. |
| [`Row`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `type Row` | Current row inside a `Rows` walk; valid until the cursor advances or closes. Typed `get_i64`, `get_f64`, `get_bool`, `get_text`, `get_blob` plus `get_opt_*` and `is_null`. |
| [`Rows`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `type Rows` | Result-set cursor. `next_row`, `columns`, `close` (idempotent). Advancing frees the previous Row; a full drain reclaims everything. For early exits, `defer rows.close()`. |
| [`Select`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `type Select` | Fluent SELECT builder. `Select::new(table).columns(&[..]).where_eq(col, sql::Value::Int(...))...render() -> String`; `.params()` returns the bound parameters. Emits Postgres-style `$N` placeholders. |
| [`Stmt`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `type Stmt` | Prepared statement. |
| [`Tx`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `type Tx` | Active transaction. `commit`, `rollback`, `savepoint`, `release_savepoint`, `rollback_to_savepoint`, `execute`. |
| [`Value`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `type Value` | Bound or fetched value. Null / Bool / Int / Float / Text / Blob. |
| [`drivers`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `fn drivers() -> Vec<String>` | Lists every currently-registered driver name. |
| [`migrate_up`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `fn migrate_up(conn: database::sql::Conn, dir: String) -> Result<i64, errors::Error>` | Applies pending forward-only schema migrations from a directory of `<version>_<slug>.sql` files. `migrate::up(&mut conn, dir)` is an equivalent namespaced spelling. |
| [`open`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `fn open(driver: String, url: String) -> Result<database::sql::Conn, errors::Error>` | Opens a database connection by driver name + URL. |
| [`register_native`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/database/sql/mod.rs) | `fn register_native(name: String, driver: database::sql::Driver) -> ()` | Registers a Gossamer-native driver under its canonical name. The driver must provide the SQL dispatch surface used by the runtime. |
