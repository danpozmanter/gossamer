# `std::os::exec`

Status: shipped

Spawn / wait for child processes (Go's os/exec shape).

## Public items

| Name | Kind | Description |
|---|---|---|
| `Command` | type | Builder for spawning a child process. |
| `Stdio` | type | Inherit / Piped / Null wiring for stdin/stdout/stderr. |
| `Output` | type | Captured stdout, stderr, and exit status from a finished child. |
| `ExitStatus` | type | Numeric exit code (None when killed by signal). |
| `Child` | type | Handle to a still-running child supporting wait / kill. |
| `Pipeline` | type | Multi-stage subprocess pipeline (stdout-to-stdin chain). |
| `Signal` | type | Portable signal selector (Term/Kill/Stop/Cont/Hup/Int/Usr1/Usr2/Pipe/Quit). |
| `run` | fn | One-shot: runs a program with args, captures stdout/stderr, returns Result<{stdout, stderr, code}, String>. |
| `spawn` | fn | Non-blocking launch; returns the child PID as Result<i64, errors::Error>. |
| `kill` | fn | Best-effort SIGTERM by pid; returns true on success. |
| `signal` | fn | Send an arbitrary signal number to a pid; returns true on success. |
| `kill_group` | fn | Send SIGTERM to the entire process group (Unix); best-effort TerminateProcess on Windows. |
| `wait_timeout` | fn | Wait up to N ms for a pid to exit; returns exit code, -1 on timeout, -2 on error. |
| `pipeline_run` | fn | Run a Vec<String> of shell-tokenised commands as a stdout-to-stdin pipeline; returns Result<Output, errors::Error>. |

