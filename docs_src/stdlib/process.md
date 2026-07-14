# `std::process`

Status: shipped

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

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/process.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Child`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/process.rs) | `type Child` | Handle to a still-running child supporting wait / kill. |
| [`abort`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/process.rs) | `fn abort() -> !` | Aborts the current process without unwinding. |
| [`exit`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/process.rs) | `fn exit(code: i64) -> !` | Exits the current process with the given status code. |
| [`id`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/process.rs) | `fn id() -> i64` | Returns the current process ID. |
| [`kill`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/process.rs) | `fn kill(pid: i64) -> bool` | Sends SIGKILL (or equivalent) to a Child. |
| [`kill_group`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/process.rs) | `fn kill_group(pid: i64) -> bool` | Sends a signal to a process group (POSIX). |
| [`pipeline_run`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/process.rs) | `fn pipeline_run(commands: Vec<String>) -> Result<process::Output, errors::Error>` | Runs a shell-tokenised pipeline and returns captured stdout/stderr plus the final exit code. |
| [`run`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/process.rs) | `fn run(program: String, args: Vec<String>) -> Result<process::Output, errors::Error>` | One-shot: runs a program with args, captures stdout/stderr plus the exit code. |
| [`signal`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/process.rs) | `fn signal(pid: i64, signum: i64) -> bool` | Sends a signal to a process by PID (POSIX). |
| [`spawn`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/process.rs) | `fn spawn(program: String, args: Vec<String>) -> Result<i64, errors::Error>` | Spawns a child process and returns its PID. |
| [`spawn_piped`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/process.rs) | `fn spawn_piped(program: String, args: Vec<String>) -> Result<process::Child, errors::Error>` | Spawns a child with piped stdin/stdout; returns Result<Child, errors::Error>. The Child's write_stdin / close_stdin / read_line / read_stdout / wait / kill methods drive it interactively. |
| [`wait_timeout`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/process.rs) | `fn wait_timeout(pid: i64, ms: i64) -> i64` | Waits for a child with a timeout (POSIX). |
