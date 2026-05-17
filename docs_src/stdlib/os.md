# `std::os`

Status: shipped

Operating-system identity and deprecated re-exports of env/process/fs.

## Public items

| Name | Kind | Description |
|---|---|---|
| `family` | fn | Returns "unix" or "windows" for the running OS family. |
| `arch` | fn | Returns the target CPU architecture (e.g. "x86_64"). |
| `args` | fn | Deprecated: use env::args. |
| `program_name` | fn | Deprecated: use env::program_name. |
| `env` | fn | Deprecated: use env::var. |
| `set_env` | fn | Deprecated: use env::set_var. |
| `exit` | fn | Deprecated: use process::exit. |
| `open` | fn | Deprecated: use fs::open. |
| `create` | fn | Deprecated: use fs::create. |
| `read_file` | fn | Deprecated: use fs::read. |
| `read_file_to_string` | fn | Deprecated: use fs::read_to_string. |
| `write_file` | fn | Deprecated: use fs::write. |
| `remove_file` | fn | Deprecated: use fs::remove_file. |
| `rename` | fn | Deprecated: use fs::rename. |
| `exists` | fn | Deprecated: use fs::exists. |
| `mkdir` | fn | Deprecated: use fs::create_dir. |
| `mkdir_all` | fn | Deprecated: use fs::create_dir_all. |
| `read_dir` | fn | Deprecated: use fs::read_dir. |
| `File` | type | Deprecated: use fs::File. |

