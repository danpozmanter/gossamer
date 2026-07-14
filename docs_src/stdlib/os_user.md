# `std::os::user`

Status: shipped

POSIX user / group lookup. Unix-backed by `nix`; Windows falls back to env vars.

## Public items

| Name | Kind | Description |
|---|---|---|
| `current_name` | fn | Login name of the current process user, or empty string. |
| `current_uid` | fn | uid of the current process user, or -1 on non-unix. |
| `current_gid` | fn | gid of the current process user, or -1 on non-unix. |
| `current_home` | fn | Home directory of the current process user. |
| `lookup_uid` | fn | Login name for the given uid, or empty string if unknown. |
| `lookup_name` | fn | uid for the user with the given login name, or -1 if not found. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os_user.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`current_gid`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os_user.rs) | `fn current_gid() -> i64` | gid of the current process user, or -1 on non-unix. |
| [`current_home`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os_user.rs) | `fn current_home() -> String` | Home directory of the current process user. |
| [`current_name`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os_user.rs) | `fn current_name() -> String` | Login name of the current process user, or empty string. |
| [`current_uid`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os_user.rs) | `fn current_uid() -> i64` | uid of the current process user, or -1 on non-unix. |
| [`lookup_name`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os_user.rs) | `fn lookup_name(name: String) -> i64` | uid for the user with the given login name, or -1 if not found. |
| [`lookup_uid`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/os_user.rs) | `fn lookup_uid(uid: i64) -> String` | Login name for the given uid, or empty string if unknown. |
