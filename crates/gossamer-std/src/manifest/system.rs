#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::option_if_let_else,
    clippy::match_same_arms,
    clippy::if_not_else,
    clippy::single_match_else,
    clippy::needless_pass_by_value,
    clippy::manual_let_else,
    clippy::redundant_else,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::map_unwrap_or,
    clippy::struct_excessive_bools,
    clippy::module_name_repetitions,
    clippy::unnecessary_wraps,
    clippy::large_enum_variant,
    clippy::if_same_then_else,
    clippy::single_match,
    clippy::useless_conversion,
    clippy::needless_borrows_for_generic_args,
    clippy::let_and_return,
    clippy::needless_collect,
    clippy::elidable_lifetime_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::needless_range_loop,
    clippy::ptr_arg,
    clippy::ptr_as_ptr,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::single_call_fn,
    clippy::unused_self,
    clippy::range_plus_one,
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_ptr_alignment,
    clippy::manual_assert,
    clippy::manual_string_new,
    clippy::match_bool,
    clippy::nonminimal_bool,
    clippy::redundant_pattern_matching,
    clippy::useless_let_if_seq
)]
//! Static manifest of every registered stdlib module.
//! Each stdlib milestone extends this table with
//! the modules it adds. Entries are listed in phase-introduction order
//! so a `gos doc` walk renders modules in the same sequence as the
//! implementation plan.

#![forbid(unsafe_code)]
use crate::registry::{StdItem, StdItemKind, StdModule};

use super::*;

pub const OS_EXEC: StdModule = StdModule {
    path: "std::os::exec",
    summary: "Spawn / wait for child processes (Go's os/exec shape).",
    items: &[
        StdItem {
            name: "Command",
            kind: StdItemKind::Type,
            doc: "Builder for spawning a child process.",
        },
        StdItem {
            name: "Stdio",
            kind: StdItemKind::Type,
            doc: "Inherit / Piped / Null wiring for stdin/stdout/stderr.",
        },
        StdItem {
            name: "Output",
            kind: StdItemKind::Type,
            doc: "Captured stdout, stderr, and exit status from a finished child.",
        },
        StdItem {
            name: "ExitStatus",
            kind: StdItemKind::Type,
            doc: "Numeric exit code (None when killed by signal).",
        },
        StdItem {
            name: "Child",
            kind: StdItemKind::Type,
            doc: "Handle to a still-running child supporting wait / kill.",
        },
        StdItem {
            name: "run",
            kind: StdItemKind::Function,
            doc: "One-shot: runs a program with args, captures stdout/stderr, returns Result<{stdout, stderr, code}, String>.",
        },
    ],
};

pub const OS_SIGNAL: StdModule = StdModule {
    path: "std::os::signal",
    summary: "POSIX-style signal subscription (Go's os/signal shape).",
    items: &[
        StdItem {
            name: "Signal",
            kind: StdItemKind::Type,
            doc: "Opaque signal name; constructors live in `sigs`.",
        },
        StdItem {
            name: "Notifier",
            kind: StdItemKind::Type,
            doc: "Returned by `on(sig)`; supports wait / try_wait.",
        },
        StdItem {
            name: "on",
            kind: StdItemKind::Function,
            doc: "Subscribes to a signal; returns a Notifier.",
        },
        StdItem {
            name: "deliver",
            kind: StdItemKind::Function,
            doc: "Test helper: synthesise a signal delivery without involving the OS.",
        },
    ],
};

pub const PATH: StdModule = StdModule {
    path: "std::path",
    summary: "POSIX-style path manipulation.",
    items: &[
        StdItem {
            name: "join",
            kind: StdItemKind::Function,
            doc: "Joins two path fragments.",
        },
        StdItem {
            name: "split",
            kind: StdItemKind::Function,
            doc: "Returns (dir, file) for the supplied path.",
        },
        StdItem {
            name: "base",
            kind: StdItemKind::Function,
            doc: "Final path segment.",
        },
        StdItem {
            name: "dir",
            kind: StdItemKind::Function,
            doc: "Directory portion.",
        },
        StdItem {
            name: "ext",
            kind: StdItemKind::Function,
            doc: "Dotted extension, if any.",
        },
        StdItem {
            name: "clean",
            kind: StdItemKind::Function,
            doc: "Collapses `.`, `..`, and duplicate separators.",
        },
    ],
};

pub const PATH_NATIVE: StdModule = StdModule {
    path: "std::path::native",
    summary: "Native-separator wrappers over `std::path` (backslash on Windows).",
    items: &[
        StdItem {
            name: "SEPARATOR",
            kind: StdItemKind::Const,
            doc: "Platform-preferred path separator character.",
        },
        StdItem {
            name: "join",
            kind: StdItemKind::Function,
            doc: "Joins two components using the platform separator.",
        },
        StdItem {
            name: "clean",
            kind: StdItemKind::Function,
            doc: "Canonicalises a path into native-separator form.",
        },
        StdItem {
            name: "to_posix",
            kind: StdItemKind::Function,
            doc: "Rewrites a native-separator path into posix form.",
        },
        StdItem {
            name: "to_native",
            kind: StdItemKind::Function,
            doc: "Rewrites a posix path into native-separator form.",
        },
    ],
};

pub const FS: StdModule = StdModule {
    path: "std::fs",
    summary: "Filesystem reading, writing, and traversal (Rust std::fs shape).",
    items: &[
        StdItem {
            name: "read",
            kind: StdItemKind::Function,
            doc: "Reads an entire file into memory as bytes.",
        },
        StdItem {
            name: "read_to_string",
            kind: StdItemKind::Function,
            doc: "Reads an entire file into memory as UTF-8 text.",
        },
        StdItem {
            name: "write",
            kind: StdItemKind::Function,
            doc: "Writes bytes to a file, creating or truncating it.",
        },
        StdItem {
            name: "read_dir",
            kind: StdItemKind::Function,
            doc: "Returns the immediate children of a directory.",
        },
        StdItem {
            name: "walk_dir",
            kind: StdItemKind::Function,
            doc: "Recursively visits every descendant entry.",
        },
        StdItem {
            name: "create_dir",
            kind: StdItemKind::Function,
            doc: "Creates a single directory. Fails if any parent is missing.",
        },
        StdItem {
            name: "create_dir_all",
            kind: StdItemKind::Function,
            doc: "Creates a directory and any missing ancestors.",
        },
        StdItem {
            name: "remove_file",
            kind: StdItemKind::Function,
            doc: "Removes a single file.",
        },
        StdItem {
            name: "remove_dir",
            kind: StdItemKind::Function,
            doc: "Removes an empty directory.",
        },
        StdItem {
            name: "remove_dir_all",
            kind: StdItemKind::Function,
            doc: "Recursively removes a directory and its contents.",
        },
        StdItem {
            name: "remove_all",
            kind: StdItemKind::Function,
            doc: "Deletes a file or a directory tree.",
        },
        StdItem {
            name: "copy",
            kind: StdItemKind::Function,
            doc: "Copies a file, creating parent dirs as needed.",
        },
        StdItem {
            name: "rename",
            kind: StdItemKind::Function,
            doc: "Renames a file or directory.",
        },
        StdItem {
            name: "exists",
            kind: StdItemKind::Function,
            doc: "Returns whether a path exists on the filesystem.",
        },
        StdItem {
            name: "is_file",
            kind: StdItemKind::Function,
            doc: "Returns whether a path exists and is a regular file.",
        },
        StdItem {
            name: "is_dir",
            kind: StdItemKind::Function,
            doc: "Returns whether a path exists and is a directory.",
        },
        StdItem {
            name: "is_symlink",
            kind: StdItemKind::Function,
            doc: "Returns whether a path exists and is a symbolic link.",
        },
        StdItem {
            name: "file_size",
            kind: StdItemKind::Function,
            doc: "Returns the file's size in bytes; 0 on error.",
        },
        StdItem {
            name: "metadata",
            kind: StdItemKind::Function,
            doc: "Returns filesystem metadata for a path.",
        },
        StdItem {
            name: "canonicalize",
            kind: StdItemKind::Function,
            doc: "Resolves a path to an absolute, symlink-free canonical form.",
        },
        StdItem {
            name: "glob",
            kind: StdItemKind::Function,
            doc: "Returns paths matching a glob pattern (*, ?, [abc], **).",
        },
        StdItem {
            name: "eval_symlinks",
            kind: StdItemKind::Function,
            doc: "Resolves all symlinks along a path; mirrors Go's filepath.EvalSymlinks.",
        },
    ],
};

pub const BYTES: StdModule = StdModule {
    path: "std::bytes",
    summary: "Byte buffers, builders, and slice helpers.",
    items: &[
        StdItem {
            name: "Buffer",
            kind: StdItemKind::Type,
            doc: "Growable byte buffer.",
        },
        StdItem {
            name: "Builder",
            kind: StdItemKind::Type,
            doc: "Incremental string builder.",
        },
        StdItem {
            name: "index_of",
            kind: StdItemKind::Function,
            doc: "First occurrence of a byte needle.",
        },
        StdItem {
            name: "split",
            kind: StdItemKind::Function,
            doc: "Splits on every separator occurrence.",
        },
        StdItem {
            name: "replace",
            kind: StdItemKind::Function,
            doc: "Replaces every occurrence of a byte needle.",
        },
    ],
};

pub const BUFIO: StdModule = StdModule {
    path: "std::bufio",
    summary: "Buffered readers, writers, and line scanners.",
    items: &[
        StdItem {
            name: "Reader",
            kind: StdItemKind::Type,
            doc: "Buffered reader.",
        },
        StdItem {
            name: "Writer",
            kind: StdItemKind::Type,
            doc: "Buffered writer.",
        },
        StdItem {
            name: "Scanner",
            kind: StdItemKind::Type,
            doc: "Line / token scanner.",
        },
        StdItem {
            name: "read_lines",
            kind: StdItemKind::Function,
            doc: "Reads every line from a file path; one-shot convenience over the streaming Scanner.",
        },
    ],
};

pub const IO: StdModule = StdModule {
    path: "std::io",
    summary: "Stream-oriented I/O abstractions.",
    items: &[
        StdItem {
            name: "Reader",
            kind: StdItemKind::Trait,
            doc: "Pull-style byte source.",
        },
        StdItem {
            name: "Writer",
            kind: StdItemKind::Trait,
            doc: "Push-style byte sink.",
        },
        StdItem {
            name: "BufReader",
            kind: StdItemKind::Type,
            doc: "Buffered wrapper around any `Reader`.",
        },
        StdItem {
            name: "BufWriter",
            kind: StdItemKind::Type,
            doc: "Buffered wrapper around any `Writer`.",
        },
        StdItem {
            name: "stdin",
            kind: StdItemKind::Function,
            doc: "Returns a handle to the process's standard input stream.",
        },
        StdItem {
            name: "stdout",
            kind: StdItemKind::Function,
            doc: "Returns a handle to the process's standard output stream.",
        },
        StdItem {
            name: "stderr",
            kind: StdItemKind::Function,
            doc: "Returns a handle to the process's standard error stream.",
        },
        StdItem {
            name: "ReadAll",
            kind: StdItemKind::Function,
            doc: "Drains a reader to a String. Mirrors Go's io.ReadAll.",
        },
        StdItem {
            name: "Copy",
            kind: StdItemKind::Function,
            doc: "Copies all bytes from src to dst; returns the byte count.",
        },
        StdItem {
            name: "Error",
            kind: StdItemKind::Type,
            doc: "Errors raised by I/O operations.",
        },
    ],
};

pub const OS: StdModule = StdModule {
    path: "std::os",
    summary: "Operating-system identity and deprecated re-exports of env/process/fs.",
    items: &[
        StdItem {
            name: "family",
            kind: StdItemKind::Function,
            doc: "Returns \"unix\" or \"windows\" for the running OS family.",
        },
        StdItem {
            name: "arch",
            kind: StdItemKind::Function,
            doc: "Returns the target CPU architecture (e.g. \"x86_64\").",
        },
        StdItem {
            name: "args",
            kind: StdItemKind::Function,
            doc: "Deprecated: use env::args.",
        },
        StdItem {
            name: "program_name",
            kind: StdItemKind::Function,
            doc: "Deprecated: use env::program_name.",
        },
        StdItem {
            name: "env",
            kind: StdItemKind::Function,
            doc: "Deprecated: use env::var.",
        },
        StdItem {
            name: "set_env",
            kind: StdItemKind::Function,
            doc: "Deprecated: use env::set_var.",
        },
        StdItem {
            name: "exit",
            kind: StdItemKind::Function,
            doc: "Deprecated: use process::exit.",
        },
        StdItem {
            name: "open",
            kind: StdItemKind::Function,
            doc: "Deprecated: use fs::open.",
        },
        StdItem {
            name: "create",
            kind: StdItemKind::Function,
            doc: "Deprecated: use fs::create.",
        },
        StdItem {
            name: "read_file",
            kind: StdItemKind::Function,
            doc: "Deprecated: use fs::read.",
        },
        StdItem {
            name: "read_file_to_string",
            kind: StdItemKind::Function,
            doc: "Deprecated: use fs::read_to_string.",
        },
        StdItem {
            name: "write_file",
            kind: StdItemKind::Function,
            doc: "Deprecated: use fs::write.",
        },
        StdItem {
            name: "remove_file",
            kind: StdItemKind::Function,
            doc: "Deprecated: use fs::remove_file.",
        },
        StdItem {
            name: "rename",
            kind: StdItemKind::Function,
            doc: "Deprecated: use fs::rename.",
        },
        StdItem {
            name: "exists",
            kind: StdItemKind::Function,
            doc: "Deprecated: use fs::exists.",
        },
        StdItem {
            name: "mkdir",
            kind: StdItemKind::Function,
            doc: "Deprecated: use fs::create_dir.",
        },
        StdItem {
            name: "mkdir_all",
            kind: StdItemKind::Function,
            doc: "Deprecated: use fs::create_dir_all.",
        },
        StdItem {
            name: "read_dir",
            kind: StdItemKind::Function,
            doc: "Deprecated: use fs::read_dir.",
        },
        StdItem {
            name: "File",
            kind: StdItemKind::Type,
            doc: "Deprecated: use fs::File.",
        },
    ],
};

pub const PROCESS: StdModule = StdModule {
    path: "std::process",
    summary: "Spawn child processes, exit the current process (Rust std::process shape).",
    items: &[
        StdItem {
            name: "Command",
            kind: StdItemKind::Type,
            doc: "Builder for spawning a child process.",
        },
        StdItem {
            name: "Stdio",
            kind: StdItemKind::Type,
            doc: "Inherit / Piped / Null wiring for stdin/stdout/stderr.",
        },
        StdItem {
            name: "Output",
            kind: StdItemKind::Type,
            doc: "Captured stdout, stderr, and exit status from a finished child.",
        },
        StdItem {
            name: "ExitStatus",
            kind: StdItemKind::Type,
            doc: "Numeric exit code (None when killed by signal).",
        },
        StdItem {
            name: "Child",
            kind: StdItemKind::Type,
            doc: "Handle to a still-running child supporting wait / kill.",
        },
        StdItem {
            name: "run",
            kind: StdItemKind::Function,
            doc: "One-shot: runs a program with args, captures stdout/stderr, returns Output.",
        },
        StdItem {
            name: "spawn",
            kind: StdItemKind::Function,
            doc: "Spawns a child process and returns a Child handle.",
        },
        StdItem {
            name: "kill",
            kind: StdItemKind::Function,
            doc: "Sends SIGKILL (or equivalent) to a Child.",
        },
        StdItem {
            name: "exit",
            kind: StdItemKind::Function,
            doc: "Exits the current process with the given status code.",
        },
        StdItem {
            name: "id",
            kind: StdItemKind::Function,
            doc: "Returns the current process ID.",
        },
        StdItem {
            name: "abort",
            kind: StdItemKind::Function,
            doc: "Aborts the current process without unwinding.",
        },
    ],
};

pub const ENV: StdModule = StdModule {
    path: "std::env",
    summary: "Process environment, command-line arguments, working directory.",
    items: &[
        StdItem {
            name: "args",
            kind: StdItemKind::Function,
            doc: "Returns the program's command-line arguments.",
        },
        StdItem {
            name: "program_name",
            kind: StdItemKind::Function,
            doc: "Returns the path used to invoke the program (argv[0]).",
        },
        StdItem {
            name: "var",
            kind: StdItemKind::Function,
            doc: "Returns the value of an environment variable.",
        },
        StdItem {
            name: "set_var",
            kind: StdItemKind::Function,
            doc: "Sets an environment variable in the current process.",
        },
        StdItem {
            name: "unset_var",
            kind: StdItemKind::Function,
            doc: "Removes an environment variable from the current process.",
        },
        StdItem {
            name: "current_dir",
            kind: StdItemKind::Function,
            doc: "Returns the current working directory.",
        },
        StdItem {
            name: "set_current_dir",
            kind: StdItemKind::Function,
            doc: "Changes the current working directory.",
        },
        StdItem {
            name: "home_dir",
            kind: StdItemKind::Function,
            doc: "Returns the calling user's home directory if known.",
        },
        StdItem {
            name: "temp_dir",
            kind: StdItemKind::Function,
            doc: "Returns the system's temporary directory.",
        },
    ],
};

pub const OS_USER: StdModule = StdModule {
    path: "std::os::user",
    summary: "POSIX user / group lookup. Unix-backed by `nix`; Windows falls back to env vars.",
    items: &[
        StdItem {
            name: "current_name",
            kind: StdItemKind::Function,
            doc: "Login name of the current process user, or empty string.",
        },
        StdItem {
            name: "current_uid",
            kind: StdItemKind::Function,
            doc: "uid of the current process user, or -1 on non-unix.",
        },
        StdItem {
            name: "current_gid",
            kind: StdItemKind::Function,
            doc: "gid of the current process user, or -1 on non-unix.",
        },
        StdItem {
            name: "current_home",
            kind: StdItemKind::Function,
            doc: "Home directory of the current process user.",
        },
        StdItem {
            name: "lookup_uid",
            kind: StdItemKind::Function,
            doc: "Login name for the given uid, or empty string if unknown.",
        },
        StdItem {
            name: "lookup_name",
            kind: StdItemKind::Function,
            doc: "uid for the user with the given login name, or -1 if not found.",
        },
    ],
};
