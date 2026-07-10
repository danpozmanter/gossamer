# `std::fs`

Status: shipped

Filesystem reading, writing, and traversal (Rust std::fs shape).

## Public items

| Name | Kind | Description |
|---|---|---|
| `File` | type | Streaming file handle; supports read, read_to_string, write, flush, and close. |
| `OpenOptions` | type | Builder for opening files with read/write/append/create/truncate flags. |
| `open` | fn | Opens a file for streaming reads. |
| `create` | fn | Creates or truncates a file and returns a streaming file handle. |
| `read` | fn | Reads an entire file into memory as bytes. |
| `read_to_string` | fn | Reads an entire file into memory as UTF-8 text. |
| `write` | fn | Writes bytes to a file, creating or truncating it. |
| `read_dir` | fn | Returns DirInfo metadata for the immediate children of a directory. |
| `walk_dir` | fn | Recursively visits every descendant entry. |
| `create_dir` | fn | Creates a single directory. Fails if any parent is missing. |
| `create_dir_all` | fn | Creates a directory and any missing ancestors. |
| `remove_file` | fn | Removes a single file. |
| `remove_dir` | fn | Removes an empty directory. |
| `remove_dir_all` | fn | Recursively removes a directory and its contents. |
| `copy` | fn | Copies a file, creating parent dirs as needed. |
| `rename` | fn | Renames a file or directory. |
| `exists` | fn | Returns whether a path exists on the filesystem. |
| `is_file` | fn | Returns whether a path exists and is a regular file. |
| `is_dir` | fn | Returns whether a path exists and is a directory. |
| `is_symlink` | fn | Returns whether a path exists and is a symbolic link. |
| `file_size` | fn | Returns the file's size in bytes; 0 on error. |
| `metadata` | fn | Returns filesystem metadata for a path. |
| `canonicalize` | fn | Resolves a path to an absolute, symlink-free canonical form. |

