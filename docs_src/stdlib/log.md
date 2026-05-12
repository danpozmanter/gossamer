# `std::log`

Flat line-oriented logging (Go's `log` shape).

## Public items

| Name | Kind | Description |
|---|---|---|
| `println` | fn | Logs a line to the configured output. |
| `printf` | fn | Logs a pre-formatted line to the configured output. |
| `fatal` | fn | Logs and exits the process with status 1. |
| `set_output` | fn | Redirects log output to a writer. |
| `set_prefix` | fn | Sets a prefix prepended to every log line. |
| `set_flags` | fn | Configures timestamp / file:line decoration bits. |
| `flags` | fn | Returns the current decoration flag set. |

