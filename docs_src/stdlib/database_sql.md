# `std::database::sql`

Driver-pluggable SQL database access.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Driver` | trait | Database driver — opens connections. |
| `Conn` | type | Open database connection. |
| `Tx` | type | Active transaction handle. |
| `Stmt` | type | Prepared statement. |
| `Rows` | type | Result-set iterator. |
| `open` | fn | Opens a database connection by driver name + URL. |

