# `std::os::user`

Status: experimental

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

