# `std::fs`

Status: experimental

Filesystem reading, writing, and traversal (Rust std::fs shape).

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`File`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `type File` | Streaming file handle; supports read, read_to_string, write, flush, and close. |
| [`OpenOptions`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `type OpenOptions` | Builder for opening files with read/write/append/create/truncate flags. |
| [`canonicalize`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn canonicalize(path: String) -> Result<String, io::Error>` | Resolves a path to an absolute, symlink-free canonical form. |
| [`copy`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn copy(src: String, dst: String) -> Result<i64, io::Error>` | Copies a file, creating parent dirs as needed. |
| [`create`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn create(path: String) -> Result<fs::File, io::Error>` | Creates or truncates a file and returns a streaming file handle. |
| [`create_dir`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn create_dir(path: String) -> Result<(), io::Error>` | Creates a single directory. Fails if any parent is missing. |
| [`create_dir_all`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn create_dir_all(path: String) -> Result<(), io::Error>` | Creates a directory and any missing ancestors. |
| [`temp_dir`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn temp_dir(prefix: String) -> Result<String, io::Error>` | Creates a unique directory under the system temporary root. The caller removes it explicitly. |
| [`temp_file`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn temp_file(prefix: String) -> Result<(fs::File, String), io::Error>` | Creates a unique temporary file and returns its streaming handle plus path. Close and remove it explicitly. |
| [`exists`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn exists(path: String) -> bool` | Returns whether a path exists on the filesystem. |
| [`file_size`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn file_size(path: String) -> i64` | Returns the file's size in bytes; 0 on error. |
| [`is_dir`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn is_dir(path: String) -> bool` | Returns whether a path exists and is a directory. |
| [`is_file`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn is_file(path: String) -> bool` | Returns whether a path exists and is a regular file. |
| [`is_symlink`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn is_symlink(path: String) -> bool` | Returns whether a path exists and is a symbolic link. |
| [`metadata`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn metadata(path: String) -> Result<fs::Metadata, io::Error>` | Returns filesystem metadata for a path. |
| [`open`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn open(path: String) -> Result<fs::File, io::Error>` | Opens a file for streaming reads. |
| [`read`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn read(path: String) -> Result<Vec<u8>, io::Error>` | Reads an entire file into memory as bytes. |
| [`read_dir`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn read_dir(path: String) -> Result<Vec<fs::DirInfo>, io::Error>` | Returns DirInfo metadata for the immediate children of a directory. |
| [`read_to_string`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn read_to_string(path: String) -> Result<String, io::Error>` | Reads an entire file into memory as UTF-8 text. |
| [`remove_dir`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn remove_dir(path: String) -> Result<(), io::Error>` | Removes an empty directory. |
| [`remove_dir_all`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn remove_dir_all(path: String) -> Result<(), io::Error>` | Recursively removes a directory and its contents. |
| [`remove_file`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn remove_file(path: String) -> Result<(), io::Error>` | Removes a single file. |
| [`rename`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn rename(src: String, dst: String) -> Result<(), io::Error>` | Renames a file or directory. |
| [`walk_dir`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn walk_dir(path: String, visit: Fn(fs::DirInfo) -> Result<(), io::Error>) -> Result<(), io::Error>` | Recursively visits every descendant entry. |
| [`write`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/fs.rs) | `fn write(path: String, contents: Vec<u8>) -> Result<(), io::Error>` | Writes bytes to a file, creating or truncating it. |

`DirInfo.path` is safe to pass back to `fs::read_dir` and `fs::walk_dir`
even when the operating-system path is not valid UTF-8. Such paths use an
internal reversible string encoding; applications should treat the field as
an opaque filesystem path rather than user-facing display text.
