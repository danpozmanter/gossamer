# `std::os::exec`

Status: shipped

Deprecated compatibility facade for child processes; new code uses std::process.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/exec.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Child`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/exec.rs) | `type Child` | Handle to a still-running child supporting wait / kill. |
| [`Pipeline`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/exec.rs) | `type Pipeline` | Multi-stage subprocess pipeline (stdout-to-stdin chain). |
| [`Signal`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/exec.rs) | `type Signal` | Portable signal selector (Term/Kill/Stop/Cont/Hup/Int/Usr1/Usr2/Pipe/Quit). |
| [`kill`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/exec.rs) | `fn kill(pid: i64) -> bool` | Best-effort SIGTERM by pid; returns true on success. |
| [`kill_group`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/exec.rs) | `fn kill_group(pid: i64) -> bool` | Send SIGTERM to the entire process group (Unix); best-effort TerminateProcess on Windows. |
| [`pipeline_run`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/exec.rs) | `fn pipeline_run(commands: Vec<String>) -> Result<process::Output, errors::Error>` | Run a Vec<String> of shell-tokenised commands as a stdout-to-stdin pipeline. |
| [`run`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/exec.rs) | `fn run(program: String, args: Vec<String>) -> Result<process::Output, errors::Error>` | One-shot: runs a program with args, captures stdout/stderr, returns Result<{stdout, stderr, code}, String>. |
| [`signal`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/exec.rs) | `fn signal(pid: i64, signum: i64) -> bool` | Send an arbitrary signal number to a pid; returns true on success. |
| [`spawn`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/exec.rs) | `fn spawn(program: String, args: Vec<String>) -> Result<i64, errors::Error>` | Non-blocking launch; returns the child PID as Result<i64, errors::Error>. |
| [`spawn_piped`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/exec.rs) | `fn spawn_piped(program: String, args: Vec<String>) -> Result<process::Child, errors::Error>` | Spawns a child with piped stdin/stdout; returns Result<Child, errors::Error>. The Child's write_stdin / close_stdin / read_line / read_stdout / wait / kill methods drive it interactively. |
| [`wait_timeout`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/exec.rs) | `fn wait_timeout(pid: i64, ms: i64) -> i64` | Wait up to N ms for a pid to exit; returns exit code, -1 on timeout, -2 on error. |
