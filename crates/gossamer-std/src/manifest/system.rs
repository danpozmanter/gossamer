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
    summary: "Deprecated compatibility facade for child processes; new code uses std::process.",
    items: &[
        StdItem {
            name: "Path",
            kind: StdItemKind::Type,
            doc: "Immutable UTF-8 lexical path value with value-returning operations.",
        },
        StdItem {
            name: "Child",
            kind: StdItemKind::Type,
            doc: "Handle to a still-running child supporting wait / kill.",
        },
        StdItem {
            name: "Pipeline",
            kind: StdItemKind::Type,
            doc: "Multi-stage subprocess pipeline (stdout-to-stdin chain).",
        },
        StdItem {
            name: "Signal",
            kind: StdItemKind::Type,
            doc: "Portable signal selector (Term/Kill/Stop/Cont/Hup/Int/Usr1/Usr2/Pipe/Quit).",
        },
        StdItem {
            name: "run",
            kind: StdItemKind::Function,
            doc: "One-shot: runs a program with args, captures stdout/stderr, returns Result<{stdout, stderr, code}, String>.",
        },
        StdItem {
            name: "spawn",
            kind: StdItemKind::Function,
            doc: "Non-blocking launch; returns the child PID as Result<i64, errors::Error>.",
        },
        StdItem {
            name: "spawn_piped",
            kind: StdItemKind::Function,
            doc: "Spawns a child with piped stdin/stdout; returns Result<Child, errors::Error>. The Child's write_stdin / close_stdin / read_line / read_stdout / wait / kill methods drive it interactively.",
        },
        StdItem {
            name: "kill",
            kind: StdItemKind::Function,
            doc: "Best-effort SIGTERM by pid; returns true on success.",
        },
        StdItem {
            name: "signal",
            kind: StdItemKind::Function,
            doc: "Send an arbitrary signal number to a pid; returns true on success.",
        },
        StdItem {
            name: "kill_group",
            kind: StdItemKind::Function,
            doc: "Send SIGTERM to the entire process group (Unix); best-effort TerminateProcess on Windows.",
        },
        StdItem {
            name: "wait_timeout",
            kind: StdItemKind::Function,
            doc: "Wait up to N ms for a pid to exit; returns exit code, -1 on timeout, -2 on error.",
        },
        StdItem {
            name: "pipeline_run",
            kind: StdItemKind::Function,
            doc: "Run a Vec<String> of shell-tokenised commands as a stdout-to-stdin pipeline.",
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
            name: "wait",
            kind: StdItemKind::Function,
            doc: "Blocks the calling goroutine until the subscribed signal fires.",
        },
        StdItem {
            name: "try_wait",
            kind: StdItemKind::Function,
            doc: "Non-blocking poll: returns true if the subscribed signal has fired.",
        },
    ],
};

pub const PATH: StdModule = StdModule {
    path: "std::path",
    summary: "Lexical filesystem-path operations; platform path grammar, no URL parsing or I/O.",
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
            name: "parent",
            kind: StdItemKind::Function,
            doc: "Parent directory, or None at the root.",
        },
        StdItem {
            name: "file_name",
            kind: StdItemKind::Function,
            doc: "Final path component, or None.",
        },
        StdItem {
            name: "file_stem",
            kind: StdItemKind::Function,
            doc: "File name without its extension.",
        },
        StdItem {
            name: "extension",
            kind: StdItemKind::Function,
            doc: "Dotted extension as an Option.",
        },
        StdItem {
            name: "is_absolute",
            kind: StdItemKind::Function,
            doc: "Reports whether the path is absolute.",
        },
        StdItem {
            name: "normalize",
            kind: StdItemKind::Function,
            doc: "Lexically normalizes the path.",
        },
        StdItem {
            name: "starts_with",
            kind: StdItemKind::Function,
            doc: "Reports whether the path begins with a prefix component-wise.",
        },
    ],
};

pub const FS: StdModule = StdModule {
    path: "std::fs",
    summary: "Filesystem reading, writing, and traversal (Rust std::fs shape).",
    items: &[
        StdItem {
            name: "File",
            kind: StdItemKind::Type,
            doc: "Streaming file handle; supports read, read_to_string, write, flush, and close.",
        },
        StdItem {
            name: "OpenOptions",
            kind: StdItemKind::Type,
            doc: "Builder for opening files with read/write/append/create/truncate flags.",
        },
        StdItem {
            name: "open",
            kind: StdItemKind::Function,
            doc: "Opens a file for streaming reads.",
        },
        StdItem {
            name: "create",
            kind: StdItemKind::Function,
            doc: "Creates or truncates a file and returns a streaming file handle.",
        },
        StdItem {
            name: "temp_dir",
            kind: StdItemKind::Function,
            doc: "Creates a unique temporary directory; the caller removes it explicitly.",
        },
        StdItem {
            name: "temp_file",
            kind: StdItemKind::Function,
            doc: "Creates a unique temporary file and returns its handle plus path.",
        },
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
            doc: "Returns immediate children as DirInfo values. Inspect their metadata fields directly; each path can be passed back to filesystem APIs.",
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
        StdItem {
            name: "read_lines_of",
            kind: StdItemKind::Function,
            doc: "Reads every line of a file path into a Vec<String>.",
        },
        StdItem {
            name: "read_to_string",
            kind: StdItemKind::Function,
            doc: "Reads an entire file path into a String.",
        },
        StdItem {
            name: "split_whitespace",
            kind: StdItemKind::Function,
            doc: "Splits a String on runs of whitespace.",
        },
    ],
};

pub const IO: StdModule = StdModule {
    path: "std::io",
    summary: "Stream-oriented I/O abstractions and process standard streams.",
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
            doc: "Returns a handle to the process's standard input stream. Use read_line(&mut String) for interactive prompts.",
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
    summary: "Operating-system identity.",
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
    ],
};

pub const PROCESS: StdModule = StdModule {
    path: "std::process",
    summary: "Canonical process control and child-process API; std::os::exec is compatibility-only.",
    items: &[
        StdItem {
            name: "Child",
            kind: StdItemKind::Type,
            doc: "Handle to a still-running child supporting wait / kill.",
        },
        StdItem {
            name: "run",
            kind: StdItemKind::Function,
            doc: "One-shot: runs a program with args, captures stdout/stderr plus the exit code.",
        },
        StdItem {
            name: "spawn",
            kind: StdItemKind::Function,
            doc: "Spawns a child process and returns its PID.",
        },
        StdItem {
            name: "spawn_piped",
            kind: StdItemKind::Function,
            doc: "Spawns a child with piped stdin/stdout; returns Result<Child, errors::Error>. The Child's write_stdin / close_stdin / read_line / read_stdout / wait / kill methods drive it interactively.",
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
        StdItem {
            name: "signal",
            kind: StdItemKind::Function,
            doc: "Sends a signal to a process by PID (POSIX).",
        },
        StdItem {
            name: "kill_group",
            kind: StdItemKind::Function,
            doc: "Sends a signal to a process group (POSIX).",
        },
        StdItem {
            name: "wait_timeout",
            kind: StdItemKind::Function,
            doc: "Waits for a child with a timeout (POSIX).",
        },
        StdItem {
            name: "pipeline_run",
            kind: StdItemKind::Function,
            doc: "Runs a shell-tokenised pipeline and returns captured stdout/stderr plus the final exit code.",
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

pub const LIFECYCLE: StdModule = StdModule {
    path: "std::lifecycle",
    summary: "Graceful-shutdown coordinator with signal handling and sd_notify support.",
    items: &[StdItem {
        name: "Lifecycle",
        kind: StdItemKind::Type,
        doc: "Registers shutdown hooks, listens for SIGTERM / SIGINT, and notifies systemd.",
    }],
};
