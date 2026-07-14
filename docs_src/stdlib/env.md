# `std::env`

Status: experimental

Process environment, command-line arguments, working directory.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/env.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`args`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/env.rs) | `fn args() -> Vec<String>` | Returns the program's command-line arguments. |
| [`current_dir`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/env.rs) | `fn current_dir() -> Result<String, io::Error>` | Returns the current working directory. |
| [`home_dir`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/env.rs) | `fn home_dir() -> Option<String>` | Returns the calling user's home directory if known. |
| [`program_name`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/env.rs) | `fn program_name() -> String` | Returns the path used to invoke the program (argv[0]). |
| [`set_current_dir`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/env.rs) | `fn set_current_dir(path: String) -> Result<(), io::Error>` | Changes the current working directory. |
| [`set_var`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/env.rs) | `fn set_var(name: String, value: String) -> ()` | Sets an environment variable in the current process. |
| [`temp_dir`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/env.rs) | `fn temp_dir() -> String` | Returns the system's temporary directory. |
| [`unset_var`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/env.rs) | `fn unset_var(name: String) -> ()` | Removes an environment variable from the current process. |
| [`var`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/env.rs) | `fn var(name: String) -> Option<String>` | Returns the value of an environment variable. |
