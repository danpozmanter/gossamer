# `std::os::exec`

Spawn / wait for child processes (Go's os/exec shape).

## Public items

| Name | Kind | Description |
|---|---|---|
| `Command` | type | Builder for spawning a child process. |
| `Stdio` | type | Inherit / Piped / Null wiring for stdin/stdout/stderr. |
| `Output` | type | Captured stdout, stderr, and exit status from a finished child. |
| `ExitStatus` | type | Numeric exit code (None when killed by signal). |
| `Child` | type | Handle to a still-running child supporting wait / kill. |
| `run` | fn | One-shot: runs a program with args, captures stdout/stderr, returns Result<{stdout, stderr, code}, String>. |

