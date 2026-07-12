# `std::env`

Status: experimental

Process environment, command-line arguments, working directory.

## Public items

| Name | Kind | Description |
|---|---|---|
| `args` | fn | Returns the program's command-line arguments. |
| `program_name` | fn | Returns the path used to invoke the program (argv[0]). |
| `var` | fn | Returns the value of an environment variable. |
| `set_var` | fn | Sets an environment variable in the current process. |
| `unset_var` | fn | Removes an environment variable from the current process. |
| `current_dir` | fn | Returns the current working directory. |
| `set_current_dir` | fn | Changes the current working directory. |
| `home_dir` | fn | Returns the calling user's home directory if known. |
| `temp_dir` | fn | Returns the system's temporary directory. |

