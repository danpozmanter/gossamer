# `std::process`

Status: experimental

Canonical process control and child-process API; std::os::exec is compatibility-only.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Child` | type | Handle to a still-running child supporting wait / kill. |
| `run` | fn | One-shot: runs a program with args, captures stdout/stderr plus the exit code. |
| `spawn` | fn | Spawns a child process and returns its PID. |
| `spawn_piped` | fn | Spawns a child with piped stdin/stdout; returns Result<Child, errors::Error>. The Child's write_stdin / close_stdin / read_line / read_stdout / wait / kill methods drive it interactively. |
| `kill` | fn | Sends SIGKILL (or equivalent) to a Child. |
| `exit` | fn | Exits the current process with the given status code. |
| `id` | fn | Returns the current process ID. |
| `abort` | fn | Aborts the current process without unwinding. |
| `signal` | fn | Sends a signal to a process by PID (POSIX). |
| `kill_group` | fn | Sends a signal to a process group (POSIX). |
| `wait_timeout` | fn | Waits for a child with a timeout (POSIX). |
| `pipeline_run` | fn | Runs a shell-tokenised pipeline and returns captured stdout/stderr plus the final exit code. |

