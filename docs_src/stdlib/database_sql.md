# `std::database::sql`

Status: experimental

Driver-pluggable SQL database access. No driver ships in the box; bring your own (Postgres, MySQL, SQLite, ...) by registering one at startup.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Driver` | trait | Database driver — opens connections. Implementations call `register` at startup. |
| `register` | fn | Registers a `Driver` under its canonical name in the process-wide registry. |
| `drivers` | fn | Lists every currently-registered driver name. |
| `open` | fn | Opens a database connection by driver name + URL. |
| `Conn` | type | Open database connection. `prepare`, `execute`, `query`, `begin`, `begin_with`, `ping`, `execute_many`, `execute_ctx`, `query_ctx`, `interrupt`. |
| `Tx` | type | Active transaction. `commit`, `rollback`, `savepoint`, `release_savepoint`, `rollback_to_savepoint`, `execute`. |
| `Stmt` | type | Prepared statement. |
| `Rows` | type | Result-set iterator. `next_row`, `columns`. |
| `Row` | type | Current row inside a `Rows` walk. Typed `get_i64`, `get_f64`, `get_bool`, `get_text`, `get_blob` plus `get_opt_*` and `is_null`. |
| `Value` | type | Bound or fetched value. Null / Bool / Int / Float / Text / Blob. |
| `IsolationLevel` | type | Default / ReadUncommitted / ReadCommitted / RepeatableRead / Serializable. Passed to `Conn::begin_with`. |
| `Error` | type | Driver error. `Error::driver(driver, msg)` builds one; `Error::PoolExhausted` and `Error::Cancelled` are variants. |
| `Pool` | type | Connection pool. `new`, `fill`, `get` (blocks up to `acquire_timeout`), `len`. Cheap to clone. |
| `PoolConfig` | type | Pool tuning: `min`, `max`, `idle_timeout`, `max_lifetime`, `acquire_timeout`, `statement_cache`. Fluent `with_*` builders. |
| `PooledConn` | type | Connection checked out from a `Pool`; returned on drop. |
| `Select` | type | Fluent SELECT builder. `Select::new(table).columns(&[..]).where_eq(col, val).order_by(col, asc).limit(n).render() -> (sql, params)`. Emits Postgres-style `$N` placeholders. |
| `migrate` | fn | Forward-only schema migrations from a directory of `<version>_<slug>.sql` files. `migrate::up(&mut conn, dir)` applies pending migrations, each in its own transaction; `migrate::discover`, `migrate::applied`, `migrate::plan`, `migrate::init` round out the surface. |

