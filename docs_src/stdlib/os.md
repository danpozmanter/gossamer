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
| `cwd` | fn | Current working directory. |
| `stdin` | fn | Process standard input stream (Go's os.Stdin). |
| `unset_env` | fn | Removes an environment variable. |
| `is_file` | fn | Reports whether the path is a regular file. |
| `is_dir` | fn | Reports whether the path is a directory. |
| `is_symlink` | fn | Reports whether the path is a symbolic link. |
| `file_size` | fn | Size of the file in bytes. |
| `temp_dir` | fn | System temporary-file directory. |
| `canonicalize` | fn | Resolves a path to its absolute canonical form. |
| `remove_dir` | fn | Removes an empty directory. |
| `remove_dir_all` | fn | Removes a directory and its contents recursively. |
| `copy` | fn | Copies a file, returning the byte count. |

