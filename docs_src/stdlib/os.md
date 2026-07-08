# `std::os`

Status: shipped

Operating-system identity and process standard input.

## Public items

| Name | Kind | Description |
|---|---|---|
| `family` | fn | Returns "unix" or "windows" for the running OS family. |
| `arch` | fn | Returns the target CPU architecture (e.g. "x86_64"). |
| `stdin` | fn | Process standard input stream (Go's os.Stdin). |

