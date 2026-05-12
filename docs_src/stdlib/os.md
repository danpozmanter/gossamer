# `std::os`

Operating-system primitives: filesystem, env, process.

## Public items

| Name | Kind | Description |
|---|---|---|
| `args` | fn | Returns the program's command-line arguments. |
| `program_name` | fn | Returns the path used to invoke the program (argv[0]). |
| `env` | fn | Returns the value of an environment variable. |
| `set_env` | fn | Sets an environment variable in the current process. |
| `exit` | fn | Exits the process with the given status code. |
| `open` | fn | Opens a file for reading. |
| `create` | fn | Creates or truncates a file for writing. |
| `read_file` | fn | Reads an entire file into memory. |
| `write_file` | fn | Writes the given bytes to a file, creating it if needed. |
| `remove_file` | fn | Removes a file from the filesystem. |
| `rename` | fn | Renames a file or directory. |
| `exists` | fn | Returns whether a path exists. |
| `mkdir` | fn | Creates a single directory. |
| `mkdir_all` | fn | Creates a directory and any required parents. |
| `read_dir` | fn | Iterates the entries of a directory. |
| `File` | type | Open file handle supporting read/write/seek/close. |

