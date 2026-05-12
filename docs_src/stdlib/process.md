# `std::process`

Spawn child processes, exit the current process (Rust std::process shape).

## Public items

| Name | Kind | Description |
|---|---|---|
| `Command` | type | Builder for spawning a child process. |
| `Stdio` | type | Inherit / Piped / Null wiring for stdin/stdout/stderr. |
| `Output` | type | Captured stdout, stderr, and exit status from a finished child. |
| `ExitStatus` | type | Numeric exit code (None when killed by signal). |
| `Child` | type | Handle to a still-running child supporting wait / kill. |
| `run` | fn | One-shot: runs a program with args, captures stdout/stderr, returns Output. |
| `spawn` | fn | Spawns a child process and returns a Child handle. |
| `kill` | fn | Sends SIGKILL (or equivalent) to a Child. |
| `exit` | fn | Exits the current process with the given status code. |
| `id` | fn | Returns the current process ID. |
| `abort` | fn | Aborts the current process without unwinding. |

