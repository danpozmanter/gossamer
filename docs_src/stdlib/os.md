# `std::os`

Status: experimental

Operating-system identity.

## Public items

| Name | Kind | Description |
|---|---|---|
| `family` | fn | Returns "unix" or "windows" for the running OS family. |
| `arch` | fn | Returns the target CPU architecture (e.g. "x86_64"). |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`arch`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn arch() -> String` | Returns the target CPU architecture (e.g. "x86_64"). |
| [`Child`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `type` — see the source declaration | Handle to a still-running child supporting wait / kill. |
| [`Pipeline`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `type` — see the source declaration | Multi-stage subprocess pipeline (stdout-to-stdin chain). |
| [`Signal`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `type` — see the source declaration | Portable signal selector (Term/Kill/Stop/Cont/Hup/Int/Usr1/Usr2/Pipe/Quit). |
| [`kill`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn kill(pid: i64) -> bool` | Best-effort SIGTERM by pid; returns true on success. |
| [`kill_group`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn kill_group(pid: i64) -> bool` | Send SIGTERM to the entire process group (Unix); best-effort TerminateProcess on Windows. |
| [`pipeline_run`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn pipeline_run(commands: Vec<String>) -> Result<process::Output, errors::Error>` | Run a Vec<String> of shell-tokenised commands as a stdout-to-stdin pipeline. |
| [`run`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn run(program: String, args: Vec<String>) -> Result<process::Output, errors::Error>` | One-shot: runs a program with args, captures stdout/stderr, returns Result<{stdout, stderr, code}, String>. |
| [`signal`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn signal(pid: i64, signum: i64) -> bool` | Send an arbitrary signal number to a pid; returns true on success. |
| [`spawn`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn spawn(program: String, args: Vec<String>) -> Result<i64, errors::Error>` | Non-blocking launch; returns the child PID as Result<i64, errors::Error>. |
| [`spawn_piped`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn spawn_piped(program: String, args: Vec<String>) -> Result<process::Child, errors::Error>` | Spawns a child with piped stdin/stdout; returns Result<Child, errors::Error>. The Child's write_stdin / close_stdin / read_line / read_stdout / wait / kill methods drive it interactively. |
| [`wait_timeout`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn wait_timeout(pid: i64, ms: i64) -> i64` | Wait up to N ms for a pid to exit; returns exit code, -1 on timeout, -2 on error. |
| [`family`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn family() -> String` | Returns "unix" or "windows" for the running OS family. |
| [`Notifier`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `type` — see the source declaration | Returned by `on(sig)`; supports wait / try_wait. |
| [`Signal`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `type` — see the source declaration | Opaque signal name; constructors live in `sigs`. |
| [`on`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn on(signum: i64) -> os::signal::Notifier` | Subscribes to a signal; returns a Notifier. |
| [`try_wait`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn try_wait(notifier: os::signal::Notifier) -> bool` | Non-blocking poll: returns true if the subscribed signal has fired. |
| [`wait`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn wait(notifier: os::signal::Notifier) -> ()` | Blocks the calling goroutine until the subscribed signal fires. |
| [`current_gid`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn current_gid() -> i64` | gid of the current process user, or -1 on non-unix. |
| [`current_home`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn current_home() -> String` | Home directory of the current process user. |
| [`current_name`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn current_name() -> String` | Login name of the current process user, or empty string. |
| [`current_uid`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn current_uid() -> i64` | uid of the current process user, or -1 on non-unix. |
| [`lookup_name`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn lookup_name(name: String) -> i64` | uid for the user with the given login name, or -1 if not found. |
| [`lookup_uid`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os.rs) | `fn lookup_uid(uid: i64) -> String` | Login name for the given uid, or empty string if unknown. |
