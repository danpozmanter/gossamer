//! Static manifest of every registered stdlib module.
//! Each stdlib milestone extends this table with
//! the modules it adds. Entries are listed in phase-introduction order
//! so a `gos doc` walk renders modules in the same sequence as the
//! implementation plan.

#![forbid(unsafe_code)]
#![allow(clippy::items_after_statements, clippy::manual_let_else)]

use crate::registry::{StdItem, StdItemKind, StdModule};

/// Renders one stdlib module as a Markdown page (Python-style
/// per-module reference). Used by `gos doc --emit-stdlib`.
#[must_use]
pub fn render_module_markdown(module: &StdModule) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str(&format!("# `{}`\n\n", module.path));
    out.push_str(&format!("{}\n\n", module.summary));
    out.push_str("## Public items\n\n");
    out.push_str("| Name | Kind | Description |\n");
    out.push_str("|---|---|---|\n");
    for item in module.items {
        let kind = match item.kind {
            StdItemKind::Function => "fn",
            StdItemKind::Type => "type",
            StdItemKind::Trait => "trait",
            StdItemKind::Macro => "macro",
            StdItemKind::Const => "const",
        };
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            item.name,
            kind,
            item.doc.replace('|', "\\|"),
        ));
    }
    out.push('\n');
    out
}

/// Renders the `docs_src/stdlib/index.md` landing page listing
/// every module with its one-line summary.
#[must_use]
pub fn render_index_markdown() -> String {
    let mut out = String::new();
    out.push_str("# Gossamer standard library\n\n");
    out.push_str(
        "One page per module. Source is `crates/gossamer-std/src/`; \
this index is regenerated from `manifest::ALL_MODULES` by \
`gos doc --emit-stdlib`.\n\n",
    );
    out.push_str("| Module | Summary |\n");
    out.push_str("|---|---|\n");
    use std::collections::BTreeSet;
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut sorted: Vec<&StdModule> = ALL_MODULES.iter().collect();
    sorted.sort_by_key(|m| m.path);
    for m in sorted {
        if !seen.insert(m.path) {
            continue;
        }
        let slug = module_slug(m.path);
        out.push_str(&format!(
            "| [`{}`]({}.md) | {} |\n",
            m.path, slug, m.summary
        ));
    }
    out.push('\n');
    out
}

/// Canonical slug for a module path — `std::http::router`
/// becomes `http_router`.
#[must_use]
pub fn module_slug(path: &str) -> String {
    path.strip_prefix("std::")
        .unwrap_or(path)
        .replace("::", "_")
}

/// Returns every `(slug, markdown)` pair for the docs site.
/// Includes the `index` page plus one page per module.
///
/// Multiple manifest entries sharing the same module path are
/// merged into one page with the union of their item lists. The
/// historical reason was a split `ENCODING_BINARY` /
/// `ENCODING_BINARY_FULL` pair; that's gone but the merge logic
/// is cheap and stays as a safety net for future additions.
#[must_use]
pub fn render_all_docs() -> Vec<(String, String)> {
    use std::collections::BTreeMap;
    // Group items by module path, preserving insertion order.
    let mut order: Vec<&'static str> = Vec::new();
    let mut merged: BTreeMap<&'static str, (String, Vec<&'static StdItem>)> = BTreeMap::new();
    for m in ALL_MODULES {
        let entry = merged.entry(m.path).or_insert_with(|| {
            order.push(m.path);
            (m.summary.to_string(), Vec::new())
        });
        for item in m.items {
            // Dedupe by item name within the merged set.
            if !entry.1.iter().any(|i| i.name == item.name) {
                entry.1.push(item);
            }
        }
    }
    let mut out: Vec<(String, String)> = Vec::with_capacity(order.len() + 1);
    out.push(("index".to_string(), render_index_markdown()));
    for path in order {
        let (summary, items) = &merged[path];
        let synthetic = StdModule {
            path,
            summary: Box::leak(summary.clone().into_boxed_str()),
            items: Box::leak(
                items
                    .iter()
                    .map(|i| **i)
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            ),
        };
        out.push((module_slug(path), render_module_markdown(&synthetic)));
    }
    out
}

/// Single source of truth for the stdlib's public surface.
pub const ALL_MODULES: &[StdModule] = &[
    FMT,
    IO,
    OS,
    OS_EXEC,
    OS_SIGNAL,
    ENV,
    PROCESS,
    LOG,
    THREAD,
    STRINGS,
    STRCONV,
    COLLECTIONS,
    NET,
    HTTP,
    ENCODING_JSON,
    SYNC,
    TIME,
    PANIC,
    // Stream D additions (QOL_PLAN.md).
    ERRORS,
    FLAG,
    PATH,
    PATH_NATIVE,
    FS,
    BYTES,
    BUFIO,
    NET_URL,
    SLOG,
    ENCODING_BASE64,
    ENCODING_HEX,
    ENCODING_BINARY,
    CONTEXT,
    CRYPTO_RAND,
    CRYPTO_SHA256,
    CRYPTO_HMAC,
    CRYPTO_SUBTLE,
    SORT,
    UTF8,
    MATH_RAND,
    TESTING,
    RUNTIME,
    TLS,
    REGEX,
    COMPRESS_GZIP,
    // Track B additions: crypto breadth, encoding, templates, db.
    CRYPTO_SHA512,
    CRYPTO_BLAKE3,
    CRYPTO_AEAD,
    CRYPTO_ED25519,
    CRYPTO_ECDSA,
    CRYPTO_X509,
    CRYPTO_KDF,
    ENCODING_YAML,
    HTML_TEMPLATE,
    TEXT_TEMPLATE,
    DATABASE_SQL,
    // P0 gap-fill: math, unicode, utf8 expansion, encoding, strings.
    MATH,
    MATH_BITS,
    UNICODE,
    ENCODING_CSV,
    ENCODING_PEM,
    // P0 gap-fill: utf16, iter.
    UTF16,
    ITER,
    // 0.4.0 — HTTP/2 folded into HTTP per the Go model;
    // extended HTTP surface, archives, big-int, FNV, XML, base32 /
    // ascii85, insecure crypto, cipher modes.
    HTTP_ROUTER,
    HTTP_MIDDLEWARE,
    HTTP_STATIC_FILES,
    HTTP_PROXY,
    HTTP_WEBSOCKET,
    HTTP_SSE,
    HTTP_CHUNKED,
    HTTP_NATIVE_CLIENT,
    ARCHIVE_ZIP,
    ARCHIVE_TAR,
    COMPRESS_FLATE,
    COMPRESS_ZLIB,
    COMPRESS_BZIP2,
    ENCODING_XML,
    ENCODING_BASE32,
    ENCODING_ASCII85,
    HASH_FNV,
    MATH_BIG,
    CRYPTO_INSECURE,
    CRYPTO_CIPHER,
];

const OS_EXEC: StdModule = StdModule {
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

const OS_SIGNAL: StdModule = StdModule {
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

const COMPRESS_GZIP: StdModule = StdModule {
    path: "std::compress::gzip",
    summary: "gzip encoder / decoder (RFC 1952; flate2-backed).",
    items: &[
        StdItem {
            name: "Level",
            kind: StdItemKind::Type,
            doc: "Compression level (`0` store-only … `9` best); default is gzip(1)'s `6`.",
        },
        StdItem {
            name: "encode",
            kind: StdItemKind::Function,
            doc: "Compresses bytes at the supplied Level.",
        },
        StdItem {
            name: "decode",
            kind: StdItemKind::Function,
            doc: "Decompresses a gzip-formatted payload.",
        },
    ],
};

const TLS: StdModule = StdModule {
    path: "std::tls",
    summary: "TLS termination and TLS client dialling (rustls-backed). Wired through both http::Server::bind_and_run_tls and http::Client; mTLS / ALPN / SNI exposed.",
    items: &[
        StdItem {
            name: "CertKey",
            kind: StdItemKind::Type,
            doc: "PEM-encoded certificate chain + private key.",
        },
        StdItem {
            name: "ServerConfig",
            kind: StdItemKind::Type,
            doc: "Opaque server-side TLS configuration.",
        },
        StdItem {
            name: "ClientConfig",
            kind: StdItemKind::Type,
            doc: "Opaque client-side TLS configuration.",
        },
        StdItem {
            name: "server_config",
            kind: StdItemKind::Function,
            doc: "Builds a server config from a CertKey. Returns Err until rustls lands.",
        },
        StdItem {
            name: "client_config",
            kind: StdItemKind::Function,
            doc: "Builds a client config. Returns Err until rustls lands.",
        },
    ],
};

const RUNTIME: StdModule = StdModule {
    path: "std::runtime",
    summary: "Goroutine / GC / scheduler introspection and tuning.",
    items: &[
        StdItem {
            name: "max_procs",
            kind: StdItemKind::Function,
            doc: "Returns the current goroutine concurrency cap.",
        },
        StdItem {
            name: "set_max_procs",
            kind: StdItemKind::Function,
            doc: "Sets the goroutine concurrency cap (GOMAXPROCS-equivalent).",
        },
        StdItem {
            name: "num_cpus",
            kind: StdItemKind::Function,
            doc: "Logical CPU cores visible to the process.",
        },
        StdItem {
            name: "mem_stats",
            kind: StdItemKind::Function,
            doc: "Read-only snapshot of GC and allocation counters.",
        },
    ],
};

const ERRORS: StdModule = StdModule {
    path: "std::errors",
    summary: "Error construction, wrapping, and chain traversal.",
    items: &[
        StdItem {
            name: "Error",
            kind: StdItemKind::Type,
            doc: "Reference-counted error value with optional cause chain.",
        },
        StdItem {
            name: "new",
            kind: StdItemKind::Function,
            doc: "Constructs a fresh error from a message.",
        },
        StdItem {
            name: "wrap",
            kind: StdItemKind::Function,
            doc: "Wraps a cause with a higher-level message.",
        },
        StdItem {
            name: "is",
            kind: StdItemKind::Function,
            doc: "Checks whether an error's chain contains a matching message.",
        },
        StdItem {
            name: "chain",
            kind: StdItemKind::Function,
            doc: "Iterator over an error and its ancestor causes.",
        },
        StdItem {
            name: "join",
            kind: StdItemKind::Function,
            doc: "Joins a list of errors into a single piped error.",
        },
    ],
};

const FLAG: StdModule = StdModule {
    path: "std::flag",
    summary: "Batteries-included CLI argument parsing.",
    items: &[
        StdItem {
            name: "Set",
            kind: StdItemKind::Type,
            doc: "Flag definition + parsing set.",
        },
        StdItem {
            name: "Error",
            kind: StdItemKind::Type,
            doc: "Error produced while parsing flags.",
        },
    ],
};

const PATH: StdModule = StdModule {
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

const PATH_NATIVE: StdModule = StdModule {
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

const FS: StdModule = StdModule {
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

const BYTES: StdModule = StdModule {
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

const BUFIO: StdModule = StdModule {
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

const NET_URL: StdModule = StdModule {
    path: "std::net::url",
    summary: "URL parsing, rendering, and query escaping.",
    items: &[
        StdItem {
            name: "Url",
            kind: StdItemKind::Type,
            doc: "Parsed URL.",
        },
        StdItem {
            name: "query_escape",
            kind: StdItemKind::Function,
            doc: "Percent-encodes a query parameter.",
        },
        StdItem {
            name: "query_unescape",
            kind: StdItemKind::Function,
            doc: "Inverse of `query_escape`.",
        },
    ],
};

const SLOG: StdModule = StdModule {
    path: "std::slog",
    summary: "Structured, levelled logging.",
    items: &[
        StdItem {
            name: "Logger",
            kind: StdItemKind::Type,
            doc: "Logger handle.",
        },
        StdItem {
            name: "Field",
            kind: StdItemKind::Type,
            doc: "Key/value pair threaded through a logger.",
        },
        StdItem {
            name: "TextHandler",
            kind: StdItemKind::Type,
            doc: "Line-oriented handler.",
        },
        StdItem {
            name: "JsonHandler",
            kind: StdItemKind::Type,
            doc: "JSON-lines handler.",
        },
        StdItem {
            name: "info",
            kind: StdItemKind::Function,
            doc: "Logs a JSON record at INFO level. Trailing args are key/value pairs.",
        },
        StdItem {
            name: "warn",
            kind: StdItemKind::Function,
            doc: "Logs a JSON record at WARN level.",
        },
        StdItem {
            name: "error",
            kind: StdItemKind::Function,
            doc: "Logs a JSON record at ERROR level.",
        },
        StdItem {
            name: "debug",
            kind: StdItemKind::Function,
            doc: "Logs a JSON record at DEBUG level.",
        },
    ],
};

const ENCODING_BASE64: StdModule = StdModule {
    path: "std::encoding::base64",
    summary: "RFC 4648 base64 encode/decode.",
    items: &[
        StdItem {
            name: "encode",
            kind: StdItemKind::Function,
            doc: "Encodes bytes to a base64 string.",
        },
        StdItem {
            name: "decode",
            kind: StdItemKind::Function,
            doc: "Decodes a base64 string.",
        },
    ],
};

const ENCODING_HEX: StdModule = StdModule {
    path: "std::encoding::hex",
    summary: "Lowercase hex encode/decode.",
    items: &[
        StdItem {
            name: "encode",
            kind: StdItemKind::Function,
            doc: "Encodes bytes to hex.",
        },
        StdItem {
            name: "decode",
            kind: StdItemKind::Function,
            doc: "Decodes a hex string.",
        },
    ],
};

const CONTEXT: StdModule = StdModule {
    path: "std::context",
    summary: "Request-scoped cancellation, deadlines, and timeouts.",
    items: &[
        StdItem {
            name: "Context",
            kind: StdItemKind::Type,
            doc: "Cancellation-aware context handle.",
        },
        StdItem {
            name: "background",
            kind: StdItemKind::Function,
            doc: "Root context — never cancelled.",
        },
        StdItem {
            name: "with_cancel",
            kind: StdItemKind::Function,
            doc: "Child context plus explicit cancel handle.",
        },
        StdItem {
            name: "with_deadline",
            kind: StdItemKind::Function,
            doc: "Child context that cancels at the supplied instant.",
        },
        StdItem {
            name: "with_timeout",
            kind: StdItemKind::Function,
            doc: "Child context that cancels after the supplied duration.",
        },
    ],
};

const CRYPTO_RAND: StdModule = StdModule {
    path: "std::crypto::rand",
    summary: "Secure random bytes from the host CSPRNG.",
    items: &[
        StdItem {
            name: "fill",
            kind: StdItemKind::Function,
            doc: "Fills a buffer with random bytes.",
        },
        StdItem {
            name: "bytes",
            kind: StdItemKind::Function,
            doc: "Returns a fresh random byte vector.",
        },
    ],
};

const CRYPTO_SHA256: StdModule = StdModule {
    path: "std::crypto::sha256",
    summary: "SHA-256 hashing.",
    items: &[
        StdItem {
            name: "digest",
            kind: StdItemKind::Function,
            doc: "Returns the 32-byte digest of an input.",
        },
        StdItem {
            name: "hex",
            kind: StdItemKind::Function,
            doc: "Returns the digest as lowercase hex.",
        },
    ],
};

const CRYPTO_HMAC: StdModule = StdModule {
    path: "std::crypto::hmac",
    summary: "HMAC-SHA-256 keyed MACs.",
    items: &[StdItem {
        name: "sha256_mac",
        kind: StdItemKind::Function,
        doc: "HMAC-SHA-256 over a message.",
    }],
};

const CRYPTO_SUBTLE: StdModule = StdModule {
    path: "std::crypto::subtle",
    summary: "Constant-time comparison helpers.",
    items: &[StdItem {
        name: "constant_time_eq",
        kind: StdItemKind::Function,
        doc: "Compares two byte slices without data-dependent branches.",
    }],
};

const CRYPTO_SHA512: StdModule = StdModule {
    path: "std::crypto::sha512",
    summary: "SHA-512 hashing.",
    items: &[
        StdItem {
            name: "digest",
            kind: StdItemKind::Function,
            doc: "Returns the 64-byte digest of an input.",
        },
        StdItem {
            name: "hex",
            kind: StdItemKind::Function,
            doc: "Returns the digest as lowercase hex.",
        },
    ],
};

const CRYPTO_BLAKE3: StdModule = StdModule {
    path: "std::crypto::blake3",
    summary: "BLAKE3 hashing.",
    items: &[
        StdItem {
            name: "digest",
            kind: StdItemKind::Function,
            doc: "Returns the 32-byte BLAKE3 digest of an input.",
        },
        StdItem {
            name: "hex",
            kind: StdItemKind::Function,
            doc: "Returns the digest as lowercase hex.",
        },
    ],
};

const CRYPTO_AEAD: StdModule = StdModule {
    path: "std::crypto::aead",
    summary: "Authenticated encryption with associated data.",
    items: &[
        StdItem {
            name: "aes_256_gcm_seal",
            kind: StdItemKind::Function,
            doc: "AES-256-GCM seal: encrypts plaintext with key, nonce, and AAD.",
        },
        StdItem {
            name: "aes_256_gcm_open",
            kind: StdItemKind::Function,
            doc: "AES-256-GCM open: decrypts and authenticates ciphertext.",
        },
        StdItem {
            name: "chacha20_poly1305_seal",
            kind: StdItemKind::Function,
            doc: "ChaCha20-Poly1305 seal.",
        },
        StdItem {
            name: "chacha20_poly1305_open",
            kind: StdItemKind::Function,
            doc: "ChaCha20-Poly1305 open.",
        },
    ],
};

const CRYPTO_ED25519: StdModule = StdModule {
    path: "std::crypto::ed25519",
    summary: "Ed25519 digital signatures.",
    items: &[
        StdItem {
            name: "keypair",
            kind: StdItemKind::Function,
            doc: "Generates a fresh Ed25519 keypair from the host CSPRNG.",
        },
        StdItem {
            name: "sign",
            kind: StdItemKind::Function,
            doc: "Signs a message with a 32-byte secret key.",
        },
        StdItem {
            name: "verify",
            kind: StdItemKind::Function,
            doc: "Verifies a 64-byte signature against a 32-byte public key.",
        },
    ],
};

const CRYPTO_ECDSA: StdModule = StdModule {
    path: "std::crypto::ecdsa",
    summary: "ECDSA over the NIST P-256 curve.",
    items: &[
        StdItem {
            name: "keypair_pem",
            kind: StdItemKind::Function,
            doc: "Generates (secret_pem, public_pem) for a fresh P-256 keypair.",
        },
        StdItem {
            name: "sign_pem",
            kind: StdItemKind::Function,
            doc: "Signs a message with a PKCS#8-PEM-encoded P-256 secret key.",
        },
        StdItem {
            name: "verify_pem",
            kind: StdItemKind::Function,
            doc: "Verifies a DER-encoded signature against an SPKI-PEM public key.",
        },
    ],
};

const CRYPTO_X509: StdModule = StdModule {
    path: "std::crypto::x509",
    summary: "X.509 certificate parsing.",
    items: &[
        StdItem {
            name: "CertInfo",
            kind: StdItemKind::Type,
            doc: "Inspected fields of an X.509 certificate.",
        },
        StdItem {
            name: "parse_pem",
            kind: StdItemKind::Function,
            doc: "Parses one PEM-encoded certificate.",
        },
        StdItem {
            name: "parse_der",
            kind: StdItemKind::Function,
            doc: "Parses one DER-encoded certificate.",
        },
    ],
};

const CRYPTO_KDF: StdModule = StdModule {
    path: "std::crypto::kdf",
    summary: "Password-based key-derivation functions.",
    items: &[
        StdItem {
            name: "pbkdf2_sha256",
            kind: StdItemKind::Function,
            doc: "PBKDF2-HMAC-SHA256 KDF.",
        },
        StdItem {
            name: "scrypt_interactive",
            kind: StdItemKind::Function,
            doc: "scrypt with the standard interactive parameters.",
        },
        StdItem {
            name: "argon2id_hash",
            kind: StdItemKind::Function,
            doc: "Argon2id PHC-format password hash.",
        },
        StdItem {
            name: "argon2id_verify",
            kind: StdItemKind::Function,
            doc: "Verifies a password against an Argon2id PHC string.",
        },
    ],
};

const ENCODING_YAML: StdModule = StdModule {
    path: "std::encoding::yaml",
    summary: "YAML 1.2 parser/emitter (serde_yaml-backed).",
    items: &[
        StdItem {
            name: "Value",
            kind: StdItemKind::Type,
            doc: "Dynamically typed YAML value.",
        },
        StdItem {
            name: "parse",
            kind: StdItemKind::Function,
            doc: "Parses a YAML document into a Value.",
        },
        StdItem {
            name: "encode",
            kind: StdItemKind::Function,
            doc: "Encodes a Value as a YAML document.",
        },
    ],
};

const HTML_TEMPLATE: StdModule = StdModule {
    path: "std::html::template",
    summary: "Context-aware HTML templates with auto-escape.",
    items: &[
        StdItem {
            name: "Template",
            kind: StdItemKind::Type,
            doc: "Compiled HTML template.",
        },
        StdItem {
            name: "parse",
            kind: StdItemKind::Function,
            doc: "Parses a template string.",
        },
        StdItem {
            name: "render",
            kind: StdItemKind::Function,
            doc: "Renders a template with the supplied data context.",
        },
    ],
};

const TEXT_TEMPLATE: StdModule = StdModule {
    path: "std::text::template",
    summary: "Plain-text templates (no escaping).",
    items: &[
        StdItem {
            name: "Template",
            kind: StdItemKind::Type,
            doc: "Compiled text template.",
        },
        StdItem {
            name: "parse",
            kind: StdItemKind::Function,
            doc: "Parses a template string.",
        },
        StdItem {
            name: "render",
            kind: StdItemKind::Function,
            doc: "Renders a template with the supplied data context.",
        },
    ],
};

const DATABASE_SQL: StdModule = StdModule {
    path: "std::database::sql",
    summary: "Driver-pluggable SQL database access.",
    items: &[
        StdItem {
            name: "Driver",
            kind: StdItemKind::Trait,
            doc: "Database driver — opens connections.",
        },
        StdItem {
            name: "Conn",
            kind: StdItemKind::Type,
            doc: "Open database connection.",
        },
        StdItem {
            name: "Tx",
            kind: StdItemKind::Type,
            doc: "Active transaction handle.",
        },
        StdItem {
            name: "Stmt",
            kind: StdItemKind::Type,
            doc: "Prepared statement.",
        },
        StdItem {
            name: "Rows",
            kind: StdItemKind::Type,
            doc: "Result-set iterator.",
        },
        StdItem {
            name: "open",
            kind: StdItemKind::Function,
            doc: "Opens a database connection by driver name + URL.",
        },
    ],
};

const SORT: StdModule = StdModule {
    path: "std::sort",
    summary: "Slice sorting and binary search.",
    items: &[
        StdItem {
            name: "sort",
            kind: StdItemKind::Function,
            doc: "Ascending unstable sort.",
        },
        StdItem {
            name: "sort_stable",
            kind: StdItemKind::Function,
            doc: "Ascending stable sort.",
        },
        StdItem {
            name: "binary_search",
            kind: StdItemKind::Function,
            doc: "Binary search on a sorted slice.",
        },
    ],
};

const UTF8: StdModule = StdModule {
    path: "std::utf8",
    summary: "UTF-8 validation and scalar decoding.",
    items: &[
        StdItem {
            name: "is_valid",
            kind: StdItemKind::Function,
            doc: "Validates a byte slice as UTF-8.",
        },
        StdItem {
            name: "rune_count",
            kind: StdItemKind::Function,
            doc: "Counts Unicode scalar values.",
        },
    ],
};

const REGEX: StdModule = StdModule {
    path: "std::regex",
    summary: "Compiled regular expressions (Rust `regex` crate syntax; no backreferences or look-around).",
    items: &[
        StdItem {
            name: "Pattern",
            kind: StdItemKind::Type,
            doc: "Compiled pattern handle returned by `compile`.",
        },
        StdItem {
            name: "compile",
            kind: StdItemKind::Function,
            doc: "Parses a pattern into a reusable `Pattern` or returns an `Err`.",
        },
        StdItem {
            name: "is_match",
            kind: StdItemKind::Function,
            doc: "Returns whether the pattern matches anywhere in the text.",
        },
        StdItem {
            name: "find",
            kind: StdItemKind::Function,
            doc: "Returns the first match as `(start, end, text)`, or `None`.",
        },
        StdItem {
            name: "find_all",
            kind: StdItemKind::Function,
            doc: "Returns every non-overlapping match as `(start, end, text)`.",
        },
        StdItem {
            name: "captures",
            kind: StdItemKind::Function,
            doc: "Returns capture groups for the first match; index 0 is the full match.",
        },
        StdItem {
            name: "captures_all",
            kind: StdItemKind::Function,
            doc: "Returns capture groups for every match in the text.",
        },
        StdItem {
            name: "replace",
            kind: StdItemKind::Function,
            doc: "Replaces the first match with the given replacement (supports `$N`).",
        },
        StdItem {
            name: "replace_all",
            kind: StdItemKind::Function,
            doc: "Replaces every non-overlapping match.",
        },
        StdItem {
            name: "split",
            kind: StdItemKind::Function,
            doc: "Splits the text on every pattern match.",
        },
    ],
};

const MATH_RAND: StdModule = StdModule {
    path: "std::math::rand",
    summary: "Deterministic pseudo-random number generation.",
    items: &[StdItem {
        name: "Rng",
        kind: StdItemKind::Type,
        doc: "SplitMix64-based RNG.",
    }],
};

const TESTING: StdModule = StdModule {
    path: "std::testing",
    summary: "Assertions and sub-test harness helpers.",
    items: &[
        StdItem {
            name: "Runner",
            kind: StdItemKind::Type,
            doc: "Sub-test collector.",
        },
        StdItem {
            name: "check",
            kind: StdItemKind::Function,
            doc: "Asserts a condition.",
        },
        StdItem {
            name: "check_eq",
            kind: StdItemKind::Function,
            doc: "Asserts equality, rendering a diff on failure.",
        },
    ],
};

const FMT: StdModule = StdModule {
    path: "std::fmt",
    summary: "Formatted printing and string interpolation.",
    items: &[
        StdItem {
            name: "Display",
            kind: StdItemKind::Trait,
            doc: "Trait for human-readable string conversion.",
        },
        StdItem {
            name: "Debug",
            kind: StdItemKind::Trait,
            doc: "Trait for debugging-oriented string conversion.",
        },
        StdItem {
            name: "println",
            kind: StdItemKind::Macro,
            doc: "Prints to stdout followed by a newline.",
        },
        StdItem {
            name: "print",
            kind: StdItemKind::Macro,
            doc: "Prints to stdout without a trailing newline.",
        },
        StdItem {
            name: "eprintln",
            kind: StdItemKind::Macro,
            doc: "Prints to stderr followed by a newline.",
        },
        StdItem {
            name: "eprint",
            kind: StdItemKind::Macro,
            doc: "Prints to stderr without a trailing newline.",
        },
        StdItem {
            name: "format",
            kind: StdItemKind::Macro,
            doc: "Formats arguments into an owned `String`.",
        },
        StdItem {
            name: "write",
            kind: StdItemKind::Macro,
            doc: "Writes formatted output into a `Writer`.",
        },
        StdItem {
            name: "writeln",
            kind: StdItemKind::Macro,
            doc: "Writes formatted output into a `Writer` followed by a newline.",
        },
    ],
};

const IO: StdModule = StdModule {
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
            name: "Error",
            kind: StdItemKind::Type,
            doc: "Errors raised by I/O operations.",
        },
    ],
};

const OS: StdModule = StdModule {
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

const PROCESS: StdModule = StdModule {
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

const LOG: StdModule = StdModule {
    path: "std::log",
    summary: "Flat line-oriented logging (Go's `log` shape).",
    items: &[
        StdItem {
            name: "println",
            kind: StdItemKind::Function,
            doc: "Logs a line to the configured output.",
        },
        StdItem {
            name: "printf",
            kind: StdItemKind::Function,
            doc: "Logs a pre-formatted line to the configured output.",
        },
        StdItem {
            name: "fatal",
            kind: StdItemKind::Function,
            doc: "Logs and exits the process with status 1.",
        },
        StdItem {
            name: "set_output",
            kind: StdItemKind::Function,
            doc: "Redirects log output to a writer.",
        },
        StdItem {
            name: "set_prefix",
            kind: StdItemKind::Function,
            doc: "Sets a prefix prepended to every log line.",
        },
        StdItem {
            name: "set_flags",
            kind: StdItemKind::Function,
            doc: "Configures timestamp / file:line decoration bits.",
        },
        StdItem {
            name: "flags",
            kind: StdItemKind::Function,
            doc: "Returns the current decoration flag set.",
        },
    ],
};

const THREAD: StdModule = StdModule {
    path: "std::thread",
    summary: "Native OS threads. For goroutines use the `go expr` syntax.",
    items: &[
        StdItem {
            name: "spawn",
            kind: StdItemKind::Function,
            doc: "Spawns a new OS thread; returns a JoinHandle.",
        },
        StdItem {
            name: "JoinHandle",
            kind: StdItemKind::Type,
            doc: "Owned handle to a spawned OS thread.",
        },
        StdItem {
            name: "join",
            kind: StdItemKind::Function,
            doc: "Waits for the thread to finish; returns its result.",
        },
        StdItem {
            name: "yield_now",
            kind: StdItemKind::Function,
            doc: "Hints to the scheduler to switch to another runnable thread.",
        },
        StdItem {
            name: "num_cpus",
            kind: StdItemKind::Function,
            doc: "Returns the number of logical CPUs available.",
        },
    ],
};

const ENV: StdModule = StdModule {
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

const STRINGS: StdModule = StdModule {
    path: "std::strings",
    summary: "Polished `String` operations.",
    items: &[
        StdItem {
            name: "split",
            kind: StdItemKind::Function,
            doc: "Splits a string by a delimiter.",
        },
        StdItem {
            name: "splitn",
            kind: StdItemKind::Function,
            doc: "Splits a string into at most `n` parts.",
        },
        StdItem {
            name: "trim",
            kind: StdItemKind::Function,
            doc: "Removes leading and trailing whitespace.",
        },
        StdItem {
            name: "contains",
            kind: StdItemKind::Function,
            doc: "Returns whether the string contains a substring.",
        },
        StdItem {
            name: "find",
            kind: StdItemKind::Function,
            doc: "Returns the byte position of the first match.",
        },
        StdItem {
            name: "replace",
            kind: StdItemKind::Function,
            doc: "Replaces every occurrence of `from` with `to`.",
        },
        StdItem {
            name: "to_lower",
            kind: StdItemKind::Function,
            doc: "Lowercases every character.",
        },
        StdItem {
            name: "to_upper",
            kind: StdItemKind::Function,
            doc: "Uppercases every character.",
        },
        StdItem {
            name: "to_lowercase",
            kind: StdItemKind::Function,
            doc: "Alias for to_lower (Rust-style name).",
        },
        StdItem {
            name: "to_uppercase",
            kind: StdItemKind::Function,
            doc: "Alias for to_upper (Rust-style name).",
        },
        StdItem {
            name: "starts_with",
            kind: StdItemKind::Function,
            doc: "Returns whether the string starts with the given prefix.",
        },
        StdItem {
            name: "ends_with",
            kind: StdItemKind::Function,
            doc: "Returns whether the string ends with the given suffix.",
        },
    ],
};

const STRCONV: StdModule = StdModule {
    path: "std::strconv",
    summary: "Conversions between strings and primitive numeric types.",
    items: &[
        StdItem {
            name: "parse_i64",
            kind: StdItemKind::Function,
            doc: "Parses a decimal `i64`.",
        },
        StdItem {
            name: "parse_u64",
            kind: StdItemKind::Function,
            doc: "Parses a decimal `u64`.",
        },
        StdItem {
            name: "parse_f64",
            kind: StdItemKind::Function,
            doc: "Parses a decimal `f64`.",
        },
        StdItem {
            name: "parse_bool",
            kind: StdItemKind::Function,
            doc: "Parses `\"true\"` / `\"false\"` into a bool.",
        },
        StdItem {
            name: "format_i64",
            kind: StdItemKind::Function,
            doc: "Renders an `i64` as a decimal string.",
        },
        StdItem {
            name: "format_f64",
            kind: StdItemKind::Function,
            doc: "Renders an `f64` as a decimal string.",
        },
        // Shorter aliases — SKILL.md and Go's `strconv` use these.
        StdItem {
            name: "parse_int",
            kind: StdItemKind::Function,
            doc: "Alias for parse_i64.",
        },
        StdItem {
            name: "atoi",
            kind: StdItemKind::Function,
            doc: "Alias for parse_i64 (Go-style spelling).",
        },
        StdItem {
            name: "parse_float",
            kind: StdItemKind::Function,
            doc: "Alias for parse_f64.",
        },
        StdItem {
            name: "format_int",
            kind: StdItemKind::Function,
            doc: "Alias for format_i64.",
        },
        StdItem {
            name: "itoa",
            kind: StdItemKind::Function,
            doc: "Alias for format_i64 (Go-style spelling).",
        },
        StdItem {
            name: "format_float",
            kind: StdItemKind::Function,
            doc: "Alias for format_f64.",
        },
    ],
};

const COLLECTIONS: StdModule = StdModule {
    path: "std::collections",
    summary: "Built-in container types.",
    items: &[
        StdItem {
            name: "Vec",
            kind: StdItemKind::Type,
            doc: "Growable contiguous sequence.",
        },
        StdItem {
            name: "VecDeque",
            kind: StdItemKind::Type,
            doc: "Double-ended queue backed by a ring buffer.",
        },
        StdItem {
            name: "HashMap",
            kind: StdItemKind::Type,
            doc: "Hash map backed by the swiss-table layout.",
        },
        StdItem {
            name: "BTreeMap",
            kind: StdItemKind::Type,
            doc: "Ordered map.",
        },
        StdItem {
            name: "HashSet",
            kind: StdItemKind::Type,
            doc: "Unordered set built on top of `HashMap`.",
        },
        StdItem {
            name: "BTreeSet",
            kind: StdItemKind::Type,
            doc: "Ordered set built on top of `BTreeMap`.",
        },
    ],
};

const NET: StdModule = StdModule {
    path: "std::net",
    summary: "TCP/UDP networking primitives.",
    items: &[
        StdItem {
            name: "TcpListener",
            kind: StdItemKind::Type,
            doc: "Accepts incoming TCP connections.",
        },
        StdItem {
            name: "TcpStream",
            kind: StdItemKind::Type,
            doc: "Bidirectional TCP byte stream.",
        },
        StdItem {
            name: "UdpSocket",
            kind: StdItemKind::Type,
            doc: "Bound UDP socket for datagram I/O.",
        },
        StdItem {
            name: "resolve",
            kind: StdItemKind::Function,
            doc: "Resolves a hostname to a list of IP addresses.",
        },
    ],
};

const HTTP: StdModule = StdModule {
    path: "std::http",
    summary: "HTTP/1.1 and HTTP/2 client and server. HTTP/2 negotiates via ALPN over TLS automatically (Go-style); h2c entry points are explicit.",
    items: &[
        StdItem {
            name: "Request",
            kind: StdItemKind::Type,
            doc: "HTTP request value passed to a handler.",
        },
        StdItem {
            name: "Response",
            kind: StdItemKind::Type,
            doc: "HTTP response value returned from a handler.",
        },
        StdItem {
            name: "Method",
            kind: StdItemKind::Type,
            doc: "HTTP method enumeration.",
        },
        StdItem {
            name: "StatusCode",
            kind: StdItemKind::Type,
            doc: "HTTP status code.",
        },
        StdItem {
            name: "Headers",
            kind: StdItemKind::Type,
            doc: "Case-insensitive header map.",
        },
        StdItem {
            name: "Server",
            kind: StdItemKind::Type,
            doc: "HTTP server bound to a TCP listener.",
        },
        StdItem {
            name: "serve",
            kind: StdItemKind::Function,
            doc: "Convenience: bind and serve an HTTP handler.",
        },
        StdItem {
            name: "Client",
            kind: StdItemKind::Type,
            doc: "HTTP client capable of GET/POST/PUT/DELETE.",
        },
        // HTTP/2 surface — folded in per the Go model.
        StdItem {
            name: "Http2Handler",
            kind: StdItemKind::Trait,
            doc: "Bounded-body HTTP/2 handler: serve(Request) -> Response.",
        },
        StdItem {
            name: "Http2StreamingHandler",
            kind: StdItemKind::Trait,
            doc: "Chunked-body HTTP/2 handler: serve(Request, StreamingResponseWriter) -> Result.",
        },
        StdItem {
            name: "StreamingResponseWriter",
            kind: StdItemKind::Type,
            doc: "Streaming HTTP/2 response writer; set_status / header / write_chunk / finish.",
        },
        StdItem {
            name: "Http2Config",
            kind: StdItemKind::Type,
            doc: "Per-connection HTTP/2 tuning (window sizes, max concurrent streams, frame caps).",
        },
        StdItem {
            name: "Http2ServerHandle",
            kind: StdItemKind::Type,
            doc: "Handle to a running HTTP/2 connection for shutdown / in-flight counts.",
        },
        StdItem {
            name: "Http2Error",
            kind: StdItemKind::Type,
            doc: "HTTP/2 server error: Io, Protocol, Handler.",
        },
        StdItem {
            name: "serve_h2_connection",
            kind: StdItemKind::Function,
            doc: "Drive an HTTP/2 connection on the calling goroutine (bounded handler).",
        },
        StdItem {
            name: "serve_h2_connection_streaming",
            kind: StdItemKind::Function,
            doc: "Same shape for Http2StreamingHandler.",
        },
        StdItem {
            name: "serve_h2c",
            kind: StdItemKind::Function,
            doc: "Bind a plain-TCP listener and serve h2c (HTTP/2 cleartext).",
        },
        StdItem {
            name: "serve_h2c_streaming",
            kind: StdItemKind::Function,
            doc: "Same shape for Http2StreamingHandler.",
        },
    ],
};

const ENCODING_JSON: StdModule = StdModule {
    path: "std::encoding::json",
    summary: "JSON parser, emitter, and derive support.",
    items: &[
        StdItem {
            name: "Serialize",
            kind: StdItemKind::Trait,
            doc: "Trait for converting a value to JSON.",
        },
        StdItem {
            name: "Deserialize",
            kind: StdItemKind::Trait,
            doc: "Trait for parsing a value from JSON.",
        },
        StdItem {
            name: "encode",
            kind: StdItemKind::Function,
            doc: "Encodes a `Serialize` value as a JSON `String`.",
        },
        StdItem {
            name: "decode",
            kind: StdItemKind::Function,
            doc: "Decodes a JSON `String` into a `Deserialize` value.",
        },
        StdItem {
            name: "Value",
            kind: StdItemKind::Type,
            doc: "Dynamically typed JSON value.",
        },
        StdItem {
            name: "Error",
            kind: StdItemKind::Type,
            doc: "Error raised by encoding/decoding operations.",
        },
    ],
};

const SYNC: StdModule = StdModule {
    path: "std::sync",
    summary: "Synchronisation primitives beyond channels.",
    items: &[
        StdItem {
            name: "Mutex",
            kind: StdItemKind::Type,
            doc: "Mutual-exclusion lock.",
        },
        StdItem {
            name: "RwLock",
            kind: StdItemKind::Type,
            doc: "Reader-writer lock.",
        },
        StdItem {
            name: "Once",
            kind: StdItemKind::Type,
            doc: "One-shot initialisation latch.",
        },
        StdItem {
            name: "WaitGroup",
            kind: StdItemKind::Type,
            doc: "Counts goroutines and waits for them to finish.",
        },
        StdItem {
            name: "Barrier",
            kind: StdItemKind::Type,
            doc: "Synchronisation barrier across goroutines.",
        },
        StdItem {
            name: "AtomicI64",
            kind: StdItemKind::Type,
            doc: "Atomic 64-bit signed integer.",
        },
        StdItem {
            name: "AtomicU64",
            kind: StdItemKind::Type,
            doc: "Atomic 64-bit unsigned integer.",
        },
        StdItem {
            name: "AtomicBool",
            kind: StdItemKind::Type,
            doc: "Atomic boolean.",
        },
    ],
};

const TIME: StdModule = StdModule {
    path: "std::time",
    summary: "Wall-clock and monotonic time facilities.",
    items: &[
        StdItem {
            name: "Instant",
            kind: StdItemKind::Type,
            doc: "Monotonic point-in-time.",
        },
        StdItem {
            name: "Duration",
            kind: StdItemKind::Type,
            doc: "Difference between two `Instant`s.",
        },
        StdItem {
            name: "SystemTime",
            kind: StdItemKind::Type,
            doc: "Wall-clock point-in-time.",
        },
        StdItem {
            name: "sleep",
            kind: StdItemKind::Function,
            doc: "Suspends the current goroutine for `Duration`.",
        },
        StdItem {
            name: "now",
            kind: StdItemKind::Function,
            doc: "Returns the current monotonic `Instant`.",
        },
        StdItem {
            name: "format_rfc3339",
            kind: StdItemKind::Function,
            doc: "Formats a `SystemTime` in RFC 3339 (`YYYY-MM-DDTHH:MM:SSZ`).",
        },
        StdItem {
            name: "parse_rfc3339",
            kind: StdItemKind::Function,
            doc: "Parses an RFC 3339 timestamp into a `SystemTime`.",
        },
    ],
};

const PANIC: StdModule = StdModule {
    path: "std::panic",
    summary: "Panic / `catch_unwind` integration.",
    items: &[
        StdItem {
            name: "panic",
            kind: StdItemKind::Macro,
            doc: "Aborts the current goroutine with a message.",
        },
        StdItem {
            name: "catch_unwind",
            kind: StdItemKind::Function,
            doc: "Runs a closure, catching any panic it raises.",
        },
    ],
};

const MATH: StdModule = StdModule {
    path: "std::math",
    summary: "Mathematical constants and f64 functions (Go's math package shape).",
    items: &[
        StdItem {
            name: "PI",
            kind: StdItemKind::Const,
            doc: "Archimedes' constant π.",
        },
        StdItem {
            name: "E",
            kind: StdItemKind::Const,
            doc: "Euler's number e.",
        },
        StdItem {
            name: "SQRT_2",
            kind: StdItemKind::Const,
            doc: "√2.",
        },
        StdItem {
            name: "LN_2",
            kind: StdItemKind::Const,
            doc: "Natural log of 2.",
        },
        StdItem {
            name: "LN_10",
            kind: StdItemKind::Const,
            doc: "Natural log of 10.",
        },
        StdItem {
            name: "PHI",
            kind: StdItemKind::Const,
            doc: "Golden ratio φ.",
        },
        StdItem {
            name: "INF",
            kind: StdItemKind::Const,
            doc: "Positive infinity.",
        },
        StdItem {
            name: "abs",
            kind: StdItemKind::Function,
            doc: "Absolute value of x.",
        },
        StdItem {
            name: "sqrt",
            kind: StdItemKind::Function,
            doc: "Square root.",
        },
        StdItem {
            name: "cbrt",
            kind: StdItemKind::Function,
            doc: "Cube root.",
        },
        StdItem {
            name: "floor",
            kind: StdItemKind::Function,
            doc: "Largest integer ≤ x.",
        },
        StdItem {
            name: "ceil",
            kind: StdItemKind::Function,
            doc: "Smallest integer ≥ x.",
        },
        StdItem {
            name: "round",
            kind: StdItemKind::Function,
            doc: "Nearest integer, half away from zero.",
        },
        StdItem {
            name: "trunc",
            kind: StdItemKind::Function,
            doc: "Integer part of x.",
        },
        StdItem {
            name: "sin",
            kind: StdItemKind::Function,
            doc: "Sine (radians).",
        },
        StdItem {
            name: "cos",
            kind: StdItemKind::Function,
            doc: "Cosine (radians).",
        },
        StdItem {
            name: "tan",
            kind: StdItemKind::Function,
            doc: "Tangent (radians).",
        },
        StdItem {
            name: "asin",
            kind: StdItemKind::Function,
            doc: "Arcsine (radians).",
        },
        StdItem {
            name: "acos",
            kind: StdItemKind::Function,
            doc: "Arccosine (radians).",
        },
        StdItem {
            name: "atan",
            kind: StdItemKind::Function,
            doc: "Arctangent (radians).",
        },
        StdItem {
            name: "atan2",
            kind: StdItemKind::Function,
            doc: "Four-quadrant arctangent of y/x.",
        },
        StdItem {
            name: "exp",
            kind: StdItemKind::Function,
            doc: "e^x.",
        },
        StdItem {
            name: "exp2",
            kind: StdItemKind::Function,
            doc: "2^x.",
        },
        StdItem {
            name: "ln",
            kind: StdItemKind::Function,
            doc: "Natural logarithm.",
        },
        StdItem {
            name: "log2",
            kind: StdItemKind::Function,
            doc: "Base-2 logarithm.",
        },
        StdItem {
            name: "log10",
            kind: StdItemKind::Function,
            doc: "Base-10 logarithm.",
        },
        StdItem {
            name: "log",
            kind: StdItemKind::Function,
            doc: "Logarithm with given base.",
        },
        StdItem {
            name: "pow",
            kind: StdItemKind::Function,
            doc: "x raised to the power y.",
        },
        StdItem {
            name: "hypot",
            kind: StdItemKind::Function,
            doc: "Euclidean distance √(x²+y²).",
        },
        StdItem {
            name: "min_f64",
            kind: StdItemKind::Function,
            doc: "Lesser of two f64 values.",
        },
        StdItem {
            name: "max_f64",
            kind: StdItemKind::Function,
            doc: "Greater of two f64 values.",
        },
        StdItem {
            name: "min_i64",
            kind: StdItemKind::Function,
            doc: "Lesser of two i64 values.",
        },
        StdItem {
            name: "max_i64",
            kind: StdItemKind::Function,
            doc: "Greater of two i64 values.",
        },
        StdItem {
            name: "abs_i64",
            kind: StdItemKind::Function,
            doc: "Absolute value of an i64.",
        },
        StdItem {
            name: "fmod",
            kind: StdItemKind::Function,
            doc: "Floating-point remainder x%y.",
        },
        StdItem {
            name: "is_nan",
            kind: StdItemKind::Function,
            doc: "Reports whether x is NaN.",
        },
        StdItem {
            name: "is_inf",
            kind: StdItemKind::Function,
            doc: "Reports whether x is infinite.",
        },
        StdItem {
            name: "nan",
            kind: StdItemKind::Function,
            doc: "Returns the IEEE 754 NaN value.",
        },
        StdItem {
            name: "inf",
            kind: StdItemKind::Function,
            doc: "Returns ±infinity based on sign.",
        },
        StdItem {
            name: "copysign",
            kind: StdItemKind::Function,
            doc: "Magnitude of x with sign of y.",
        },
        StdItem {
            name: "dim",
            kind: StdItemKind::Function,
            doc: "max(x-y, 0) — Go's math.Dim.",
        },
    ],
};

const MATH_BITS: StdModule = StdModule {
    path: "std::math::bits",
    summary: "Integer bit-manipulation operations (Go's math/bits shape).",
    items: &[
        StdItem {
            name: "count_ones",
            kind: StdItemKind::Function,
            doc: "Number of set bits (popcount).",
        },
        StdItem {
            name: "count_zeros",
            kind: StdItemKind::Function,
            doc: "Number of clear bits.",
        },
        StdItem {
            name: "leading_zeros",
            kind: StdItemKind::Function,
            doc: "Leading zero bit count.",
        },
        StdItem {
            name: "trailing_zeros",
            kind: StdItemKind::Function,
            doc: "Trailing zero bit count.",
        },
        StdItem {
            name: "rotate_left",
            kind: StdItemKind::Function,
            doc: "Rotates x left by n bits.",
        },
        StdItem {
            name: "rotate_right",
            kind: StdItemKind::Function,
            doc: "Rotates x right by n bits.",
        },
        StdItem {
            name: "reverse_bits",
            kind: StdItemKind::Function,
            doc: "Reverses bit order of x.",
        },
        StdItem {
            name: "reverse_bytes",
            kind: StdItemKind::Function,
            doc: "Reverses byte order of x.",
        },
        StdItem {
            name: "len",
            kind: StdItemKind::Function,
            doc: "Minimum bits required to represent x.",
        },
        StdItem {
            name: "add",
            kind: StdItemKind::Function,
            doc: "x + y + carry; returns (sum, carry_out).",
        },
        StdItem {
            name: "sub",
            kind: StdItemKind::Function,
            doc: "x - y - borrow; returns (diff, borrow_out).",
        },
        StdItem {
            name: "mul",
            kind: StdItemKind::Function,
            doc: "Full 128-bit product; returns (hi, lo).",
        },
        StdItem {
            name: "div",
            kind: StdItemKind::Function,
            doc: "128-bit dividend / 64-bit divisor; returns (quotient, remainder).",
        },
    ],
};

const UNICODE: StdModule = StdModule {
    path: "std::unicode",
    summary: "Unicode character property predicates and casing operations.",
    items: &[
        StdItem {
            name: "is_letter",
            kind: StdItemKind::Function,
            doc: "True if r is a Unicode letter.",
        },
        StdItem {
            name: "is_digit",
            kind: StdItemKind::Function,
            doc: "True if r is a decimal digit.",
        },
        StdItem {
            name: "is_number",
            kind: StdItemKind::Function,
            doc: "True if r is a numeric character.",
        },
        StdItem {
            name: "is_space",
            kind: StdItemKind::Function,
            doc: "True if r is whitespace.",
        },
        StdItem {
            name: "is_upper",
            kind: StdItemKind::Function,
            doc: "True if r is an uppercase letter.",
        },
        StdItem {
            name: "is_lower",
            kind: StdItemKind::Function,
            doc: "True if r is a lowercase letter.",
        },
        StdItem {
            name: "is_title",
            kind: StdItemKind::Function,
            doc: "True if r is a titlecase letter.",
        },
        StdItem {
            name: "is_punct",
            kind: StdItemKind::Function,
            doc: "True if r is a punctuation character.",
        },
        StdItem {
            name: "is_symbol",
            kind: StdItemKind::Function,
            doc: "True if r is a symbol character.",
        },
        StdItem {
            name: "is_mark",
            kind: StdItemKind::Function,
            doc: "True if r is a combining mark.",
        },
        StdItem {
            name: "is_print",
            kind: StdItemKind::Function,
            doc: "True if r is a printable character.",
        },
        StdItem {
            name: "is_graphic",
            kind: StdItemKind::Function,
            doc: "True if r is a graphic character.",
        },
        StdItem {
            name: "is_control",
            kind: StdItemKind::Function,
            doc: "True if r is a control character.",
        },
        StdItem {
            name: "to_upper",
            kind: StdItemKind::Function,
            doc: "Maps r to its uppercase equivalent.",
        },
        StdItem {
            name: "to_lower",
            kind: StdItemKind::Function,
            doc: "Maps r to its lowercase equivalent.",
        },
        StdItem {
            name: "to_title",
            kind: StdItemKind::Function,
            doc: "Maps r to its titlecase equivalent.",
        },
        StdItem {
            name: "simple_fold",
            kind: StdItemKind::Function,
            doc: "Next rune in Unicode case-folding cycle.",
        },
    ],
};

const ENCODING_CSV: StdModule = StdModule {
    path: "std::encoding::csv",
    summary: "CSV record reader and writer.",
    items: &[
        StdItem {
            name: "read",
            kind: StdItemKind::Function,
            doc: "Parses all CSV records from a string.",
        },
        StdItem {
            name: "parse_line",
            kind: StdItemKind::Function,
            doc: "Parses a single CSV-formatted line.",
        },
        StdItem {
            name: "write",
            kind: StdItemKind::Function,
            doc: "Serialises records as a CSV string.",
        },
    ],
};

const ENCODING_PEM: StdModule = StdModule {
    path: "std::encoding::pem",
    summary: "PEM block encoder and decoder.",
    items: &[
        StdItem {
            name: "Block",
            kind: StdItemKind::Type,
            doc: "A decoded PEM block with type label and DER bytes.",
        },
        StdItem {
            name: "encode",
            kind: StdItemKind::Function,
            doc: "Encodes a Block as a PEM string.",
        },
        StdItem {
            name: "decode",
            kind: StdItemKind::Function,
            doc: "Decodes the first PEM block from a string.",
        },
        StdItem {
            name: "decode_all",
            kind: StdItemKind::Function,
            doc: "Decodes all PEM blocks from a string.",
        },
    ],
};

const ENCODING_BINARY: StdModule = StdModule {
    path: "std::encoding::binary",
    summary: "Big/little-endian integer packing and varint codecs.",
    items: &[
        StdItem {
            name: "get_u8",
            kind: StdItemKind::Function,
            doc: "Reads a single byte.",
        },
        StdItem {
            name: "put_u8",
            kind: StdItemKind::Function,
            doc: "Writes a single byte.",
        },
        StdItem {
            name: "get_u16_be",
            kind: StdItemKind::Function,
            doc: "Reads a big-endian u16.",
        },
        StdItem {
            name: "put_u16_be",
            kind: StdItemKind::Function,
            doc: "Writes a big-endian u16.",
        },
        StdItem {
            name: "get_u16_le",
            kind: StdItemKind::Function,
            doc: "Reads a little-endian u16.",
        },
        StdItem {
            name: "put_u16_le",
            kind: StdItemKind::Function,
            doc: "Writes a little-endian u16.",
        },
        StdItem {
            name: "get_u32_be",
            kind: StdItemKind::Function,
            doc: "Reads a big-endian u32.",
        },
        StdItem {
            name: "put_u32_be",
            kind: StdItemKind::Function,
            doc: "Writes a big-endian u32.",
        },
        StdItem {
            name: "get_u32_le",
            kind: StdItemKind::Function,
            doc: "Reads a little-endian u32.",
        },
        StdItem {
            name: "put_u32_le",
            kind: StdItemKind::Function,
            doc: "Writes a little-endian u32.",
        },
        StdItem {
            name: "get_u64_be",
            kind: StdItemKind::Function,
            doc: "Reads a big-endian u64.",
        },
        StdItem {
            name: "put_u64_be",
            kind: StdItemKind::Function,
            doc: "Writes a big-endian u64.",
        },
        StdItem {
            name: "get_u64_le",
            kind: StdItemKind::Function,
            doc: "Reads a little-endian u64.",
        },
        StdItem {
            name: "put_u64_le",
            kind: StdItemKind::Function,
            doc: "Writes a little-endian u64.",
        },
        StdItem {
            name: "uvarint",
            kind: StdItemKind::Function,
            doc: "Decodes an unsigned varint.",
        },
        StdItem {
            name: "varint",
            kind: StdItemKind::Function,
            doc: "Decodes a signed varint (zigzag).",
        },
        StdItem {
            name: "put_uvarint",
            kind: StdItemKind::Function,
            doc: "Encodes an unsigned varint.",
        },
        StdItem {
            name: "put_varint",
            kind: StdItemKind::Function,
            doc: "Encodes a signed varint (zigzag).",
        },
    ],
};

const UTF16: StdModule = StdModule {
    path: "std::utf16",
    summary: "UTF-16 encoding/decoding and surrogate pair helpers.",
    items: &[
        StdItem {
            name: "is_surrogate",
            kind: StdItemKind::Function,
            doc: "True iff r falls in the surrogate range U+D800..U+DFFF.",
        },
        StdItem {
            name: "rune_len",
            kind: StdItemKind::Function,
            doc: "Number of UTF-16 code units needed to encode r (1 or 2).",
        },
        StdItem {
            name: "encode_rune",
            kind: StdItemKind::Function,
            doc: "Encodes r as 1 or 2 u16 code units.",
        },
        StdItem {
            name: "decode_surrogate_pair",
            kind: StdItemKind::Function,
            doc: "Decodes a high+low surrogate pair to a char.",
        },
        StdItem {
            name: "append_rune",
            kind: StdItemKind::Function,
            doc: "Appends the UTF-16 encoding of r to a Vec<u16>.",
        },
        StdItem {
            name: "encode",
            kind: StdItemKind::Function,
            doc: "Encodes a []char to Vec<u16>.",
        },
        StdItem {
            name: "decode",
            kind: StdItemKind::Function,
            doc: "Decodes a []u16 to Vec<char>, replacing surrogates with U+FFFD.",
        },
        StdItem {
            name: "encode_string",
            kind: StdItemKind::Function,
            doc: "Encodes a String directly to Vec<u16>.",
        },
        StdItem {
            name: "decode_to_string",
            kind: StdItemKind::Function,
            doc: "Decodes a []u16 to String.",
        },
    ],
};

const ITER: StdModule = StdModule {
    path: "std::iter",
    summary: "Sequence adapters over Vec<T>: map, filter, fold, zip, enumerate, chain, etc.",
    items: &[
        StdItem {
            name: "count",
            kind: StdItemKind::Function,
            doc: "Number of elements.",
        },
        StdItem {
            name: "take",
            kind: StdItemKind::Function,
            doc: "First n elements.",
        },
        StdItem {
            name: "skip",
            kind: StdItemKind::Function,
            doc: "All elements after the first n.",
        },
        StdItem {
            name: "zip",
            kind: StdItemKind::Function,
            doc: "Pairs elements from two sequences.",
        },
        StdItem {
            name: "enumerate",
            kind: StdItemKind::Function,
            doc: "Pairs each element with its index.",
        },
        StdItem {
            name: "chain",
            kind: StdItemKind::Function,
            doc: "Concatenates two sequences.",
        },
        StdItem {
            name: "flatten",
            kind: StdItemKind::Function,
            doc: "Flattens a Vec<Vec<T>> into Vec<T>.",
        },
        StdItem {
            name: "reversed",
            kind: StdItemKind::Function,
            doc: "Returns a reversed copy.",
        },
        StdItem {
            name: "dedup",
            kind: StdItemKind::Function,
            doc: "Removes consecutive duplicate elements.",
        },
        StdItem {
            name: "map",
            kind: StdItemKind::Function,
            doc: "Applies f to each element, returning a new Vec.",
        },
        StdItem {
            name: "filter",
            kind: StdItemKind::Function,
            doc: "Returns elements where f is true.",
        },
        StdItem {
            name: "fold",
            kind: StdItemKind::Function,
            doc: "Reduces a sequence with an accumulator.",
        },
        StdItem {
            name: "flat_map",
            kind: StdItemKind::Function,
            doc: "Maps f and flattens one level.",
        },
        StdItem {
            name: "any",
            kind: StdItemKind::Function,
            doc: "True if any element satisfies f.",
        },
        StdItem {
            name: "all",
            kind: StdItemKind::Function,
            doc: "True if every element satisfies f.",
        },
        StdItem {
            name: "sum",
            kind: StdItemKind::Function,
            doc: "Sum of i64 or f64 elements.",
        },
    ],
};

// ---------------------------------------------------------------------------
// 0.4.0 surface — HTTP/2, websocket, sse, router, middleware, static files,
// proxy, native client, chunked transfer, archives, extended compress,
// XML / base32 / ascii85, FNV, big-int, insecure / cipher crypto.
// ---------------------------------------------------------------------------

const HTTP_ROUTER: StdModule = StdModule {
    path: "std::http::router",
    summary: "Go 1.22-class ServeMux: method-aware path patterns with parameter captures + prefix routes.",
    items: &[
        StdItem {
            name: "Router",
            kind: StdItemKind::Type,
            doc: "Routing table (Rust-side full surface; method-chain bridge is interp tier).",
        },
        StdItem {
            name: "Params",
            kind: StdItemKind::Type,
            doc: "Captured path parameters.",
        },
        StdItem {
            name: "Handler",
            kind: StdItemKind::Trait,
            doc: "Anything callable as Fn(&Request, &Params) -> Response.",
        },
        StdItem {
            name: "new",
            kind: StdItemKind::Function,
            doc: "Allocate a fresh Router handle. Interp tier.",
        },
        StdItem {
            name: "add",
            kind: StdItemKind::Function,
            doc: "Register a route: `(router, method, pattern)`. Interp tier.",
        },
        StdItem {
            name: "lookup",
            kind: StdItemKind::Function,
            doc: "Find the index of the first route matching `(method, path)`. Interp tier.",
        },
    ],
};

const HTTP_MIDDLEWARE: StdModule = StdModule {
    path: "std::http::middleware",
    summary: "Composable middleware: logger, recoverer, request_id, cors, basic_auth, compress_gzip.",
    items: &[
        StdItem {
            name: "Handler",
            kind: StdItemKind::Trait,
            doc: "Anything serving (Request, Params) -> Response.",
        },
        StdItem {
            name: "Chain",
            kind: StdItemKind::Type,
            doc: "Helper for composing middleware in a single value.",
        },
        StdItem {
            name: "logger",
            kind: StdItemKind::Function,
            doc: "Logs method path status bytes elapsed_ms per request.",
        },
        StdItem {
            name: "recoverer",
            kind: StdItemKind::Function,
            doc: "Catches handler panics; returns 500.",
        },
        StdItem {
            name: "request_id",
            kind: StdItemKind::Function,
            doc: "Stamps each response with X-Request-Id.",
        },
        StdItem {
            name: "cors",
            kind: StdItemKind::Function,
            doc: "CORS preflight + per-response headers.",
        },
        StdItem {
            name: "basic_auth",
            kind: StdItemKind::Function,
            doc: "HTTP Basic auth gate; constant-time compare.",
        },
        StdItem {
            name: "compress_gzip",
            kind: StdItemKind::Function,
            doc: "Gzips bodies above a size threshold when client advertises Accept-Encoding: gzip (Rust-side wrapper).",
        },
        StdItem {
            name: "new_request_id",
            kind: StdItemKind::Function,
            doc: "Generate a process-monotonic request id string. Available in interp + compiled.",
        },
        StdItem {
            name: "accepts_gzip",
            kind: StdItemKind::Function,
            doc: "Check an Accept-Encoding header for a gzip token. Available in interp + compiled.",
        },
        StdItem {
            name: "decode_basic_auth",
            kind: StdItemKind::Function,
            doc: "Decode a Basic-auth Authorization header into (user, password). Interp tier.",
        },
    ],
};

const HTTP_STATIC_FILES: StdModule = StdModule {
    path: "std::http::static_files",
    summary: "Caching static-file handler: ETag, Last-Modified, byte ranges, MIME sniff.",
    items: &[
        StdItem {
            name: "FileServer",
            kind: StdItemKind::Type,
            doc: "Static-file handler rooted at a directory (Rust-side; streaming).",
        },
        StdItem {
            name: "serve_file",
            kind: StdItemKind::Function,
            doc: "Read a single file and return it as a Response struct. Interp tier.",
        },
        StdItem {
            name: "mime_for_path",
            kind: StdItemKind::Function,
            doc: "Guess a MIME type from a file path's extension. Available in interp + compiled.",
        },
    ],
};

const HTTP_PROXY: StdModule = StdModule {
    path: "std::http::proxy",
    summary: "Reverse proxy on top of http::Client. Director-style request mutator + hop-by-hop strip + error handler.",
    items: &[
        StdItem {
            name: "Proxy",
            kind: StdItemKind::Type,
            doc: "Reverse-proxy handler (Rust-side).",
        },
        StdItem {
            name: "Director",
            kind: StdItemKind::Type,
            doc: "Fn(&mut Request) request mutator (Rust-side).",
        },
        StdItem {
            name: "forward",
            kind: StdItemKind::Function,
            doc: "One-shot upstream forward: `(url, method, body) -> Result<Response, Error>`. Interp tier.",
        },
    ],
};

const HTTP_WEBSOCKET: StdModule = StdModule {
    path: "std::http::websocket",
    summary: "RFC 6455 WebSocket support. Server-side accept + send_text / send_binary / ping / pong / close.",
    items: &[
        StdItem {
            name: "WebSocket",
            kind: StdItemKind::Type,
            doc: "Accepted WebSocket connection (Rust-side framing).",
        },
        StdItem {
            name: "Message",
            kind: StdItemKind::Type,
            doc: "Text / Binary / Ping / Pong / Close.",
        },
        StdItem {
            name: "accept",
            kind: StdItemKind::Function,
            doc: "Upgrade an incoming Request to a WebSocket (Rust-side).",
        },
        StdItem {
            name: "Error",
            kind: StdItemKind::Type,
            doc: "Io / Protocol / BadHandshake.",
        },
        StdItem {
            name: "accept_key",
            kind: StdItemKind::Function,
            doc: "Compute RFC 6455 Sec-WebSocket-Accept from a client nonce. Available in interp + compiled.",
        },
        StdItem {
            name: "is_websocket_upgrade",
            kind: StdItemKind::Function,
            doc: "Test whether an incoming Request carries a WebSocket upgrade handshake. Interp tier.",
        },
    ],
};

const HTTP_SSE: StdModule = StdModule {
    path: "std::http::sse",
    summary: "Server-Sent Events (text/event-stream) emitter with heartbeat ticks and retry hint.",
    items: &[
        StdItem {
            name: "Stream",
            kind: StdItemKind::Type,
            doc: "Active SSE stream — handler writes events through it (Rust-side).",
        },
        StdItem {
            name: "Event",
            kind: StdItemKind::Type,
            doc: "One SSE event (id, event, data, retry).",
        },
        StdItem {
            name: "serve",
            kind: StdItemKind::Function,
            doc: "Wraps a handler closure into an SSE response (Rust-side; streaming).",
        },
        StdItem {
            name: "encode_event",
            kind: StdItemKind::Function,
            doc: "Render one event block as a string: `(event, data, id) -> String`. Available in interp + compiled.",
        },
        StdItem {
            name: "encode_comment",
            kind: StdItemKind::Function,
            doc: "Render a `:`-prefixed keepalive line. Available in interp + compiled.",
        },
        StdItem {
            name: "encode_retry",
            kind: StdItemKind::Function,
            doc: "Render a `retry:` reconnect-hint directive in milliseconds. Available in interp + compiled.",
        },
    ],
};

const HTTP_CHUNKED: StdModule = StdModule {
    path: "std::http::chunked",
    summary: "RFC 7230 §4.1 chunked transfer-encoding reader and writer.",
    items: &[
        StdItem {
            name: "Reader",
            kind: StdItemKind::Type,
            doc: "Decodes a chunked body from any Read source (Rust-side; streaming).",
        },
        StdItem {
            name: "Writer",
            kind: StdItemKind::Type,
            doc: "Encodes raw bytes into chunked frames over any Write sink (Rust-side; streaming).",
        },
        StdItem {
            name: "encode",
            kind: StdItemKind::Function,
            doc: "One-shot: wraps a buffer in chunked transfer-encoding with terminator. Available in interp + compiled.",
        },
        StdItem {
            name: "decode",
            kind: StdItemKind::Function,
            doc: "One-shot: concatenates data chunks from a complete chunked body. Available in interp + compiled.",
        },
    ],
};

const HTTP_NATIVE_CLIENT: StdModule = StdModule {
    path: "std::http::native_client",
    summary: "Goroutine-driven HTTP/1.1 client over std::net (no ureq, no blocking pool).",
    items: &[
        StdItem {
            name: "Client",
            kind: StdItemKind::Type,
            doc: "Native h1 client (Rust-side; full builder surface).",
        },
        StdItem {
            name: "Error",
            kind: StdItemKind::Type,
            doc: "Connect / Tls / Http / Redirect / Timeout / Io.",
        },
        StdItem {
            name: "get",
            kind: StdItemKind::Function,
            doc: "One-shot GET → Result<Response, Error>. Interp tier (compiled tier shares http::get).",
        },
        StdItem {
            name: "post",
            kind: StdItemKind::Function,
            doc: "One-shot POST: `(url, body, content_type)`. Interp tier.",
        },
        StdItem {
            name: "put",
            kind: StdItemKind::Function,
            doc: "One-shot PUT: `(url, body, content_type)`. Interp tier.",
        },
        StdItem {
            name: "delete",
            kind: StdItemKind::Function,
            doc: "One-shot DELETE → Result<Response, Error>. Interp tier.",
        },
    ],
};

const ARCHIVE_ZIP: StdModule = StdModule {
    path: "std::archive::zip",
    summary: "ZIP archive reader and writer.",
    items: &[
        StdItem {
            name: "ZipEntry",
            kind: StdItemKind::Type,
            doc: "name + decompressed data + is_dir flag.",
        },
        StdItem {
            name: "read",
            kind: StdItemKind::Function,
            doc: "Reads all file entries from a zip stored in `data`.",
        },
        StdItem {
            name: "write",
            kind: StdItemKind::Function,
            doc: "Builds an in-memory zip from (name, data) pairs.",
        },
    ],
};

const ARCHIVE_TAR: StdModule = StdModule {
    path: "std::archive::tar",
    summary: "Unix tar reader and writer (USTAR / PAX-aware decode).",
    items: &[
        StdItem {
            name: "TarEntry",
            kind: StdItemKind::Type,
            doc: "name + data + size + mode.",
        },
        StdItem {
            name: "read",
            kind: StdItemKind::Function,
            doc: "Reads all entries from a tar archive.",
        },
        StdItem {
            name: "write",
            kind: StdItemKind::Function,
            doc: "Builds a tar archive from (name, data) pairs.",
        },
    ],
};

const COMPRESS_FLATE: StdModule = StdModule {
    path: "std::compress::flate",
    summary: "Raw DEFLATE (RFC 1951) encoder / decoder.",
    items: &[
        StdItem {
            name: "encode",
            kind: StdItemKind::Function,
            doc: "One-shot DEFLATE compress.",
        },
        StdItem {
            name: "decode",
            kind: StdItemKind::Function,
            doc: "One-shot DEFLATE decompress.",
        },
    ],
};

const COMPRESS_ZLIB: StdModule = StdModule {
    path: "std::compress::zlib",
    summary: "zlib (RFC 1950) encoder / decoder.",
    items: &[
        StdItem {
            name: "encode",
            kind: StdItemKind::Function,
            doc: "One-shot zlib compress.",
        },
        StdItem {
            name: "decode",
            kind: StdItemKind::Function,
            doc: "One-shot zlib decompress.",
        },
    ],
};

const COMPRESS_BZIP2: StdModule = StdModule {
    path: "std::compress::bzip2",
    summary: "bzip2 encoder / decoder (BZh format).",
    items: &[
        StdItem {
            name: "encode",
            kind: StdItemKind::Function,
            doc: "One-shot bzip2 compress.",
        },
        StdItem {
            name: "decode",
            kind: StdItemKind::Function,
            doc: "One-shot bzip2 decompress.",
        },
    ],
};

const ENCODING_XML: StdModule = StdModule {
    path: "std::encoding::xml",
    summary: "Streaming XML decoder + builder (quick-xml).",
    items: &[
        StdItem {
            name: "Reader",
            kind: StdItemKind::Type,
            doc: "Pull-style XML reader.",
        },
        StdItem {
            name: "Writer",
            kind: StdItemKind::Type,
            doc: "Streaming XML writer.",
        },
        StdItem {
            name: "Event",
            kind: StdItemKind::Type,
            doc: "Start / End / Text / CData / Comment / Eof.",
        },
    ],
};

const ENCODING_BASE32: StdModule = StdModule {
    path: "std::encoding::base32",
    summary: "RFC 4648 base32 (uppercase) encode / decode.",
    items: &[
        StdItem {
            name: "encode",
            kind: StdItemKind::Function,
            doc: "Bytes -> base32 string.",
        },
        StdItem {
            name: "encode_padded",
            kind: StdItemKind::Function,
            doc: "With explicit = padding.",
        },
        StdItem {
            name: "decode",
            kind: StdItemKind::Function,
            doc: "Base32 string -> bytes.",
        },
    ],
};

const ENCODING_ASCII85: StdModule = StdModule {
    path: "std::encoding::ascii85",
    summary: "ASCII85 / base85 encode / decode.",
    items: &[
        StdItem {
            name: "encode",
            kind: StdItemKind::Function,
            doc: "Bytes -> ASCII85 string.",
        },
        StdItem {
            name: "decode",
            kind: StdItemKind::Function,
            doc: "ASCII85 string -> bytes.",
        },
    ],
};

const HASH_FNV: StdModule = StdModule {
    path: "std::hash::fnv",
    summary: "FNV-1a non-cryptographic hash (32-bit, 64-bit).",
    items: &[
        StdItem {
            name: "fnv1a_32",
            kind: StdItemKind::Function,
            doc: "One-shot 32-bit FNV-1a of a byte slice.",
        },
        StdItem {
            name: "fnv1a_64",
            kind: StdItemKind::Function,
            doc: "One-shot 64-bit FNV-1a of a byte slice.",
        },
    ],
};

const MATH_BIG: StdModule = StdModule {
    path: "std::math::big",
    summary: "Arbitrary-precision integers (num-bigint).",
    items: &[
        StdItem {
            name: "Int",
            kind: StdItemKind::Type,
            doc: "Arbitrary-precision signed integer.",
        },
        StdItem {
            name: "Uint",
            kind: StdItemKind::Type,
            doc: "Arbitrary-precision unsigned integer.",
        },
        StdItem {
            name: "factorial",
            kind: StdItemKind::Function,
            doc: "Computes n! as an Int.",
        },
    ],
};

const CRYPTO_INSECURE: StdModule = StdModule {
    path: "std::crypto::insecure",
    summary: "Legacy / broken hashes (MD5, SHA-1). Compat only — never use for new code.",
    items: &[
        StdItem {
            name: "md5",
            kind: StdItemKind::Function,
            doc: "One-shot MD5.",
        },
        StdItem {
            name: "sha1",
            kind: StdItemKind::Function,
            doc: "One-shot SHA-1.",
        },
    ],
};

const CRYPTO_CIPHER: StdModule = StdModule {
    path: "std::crypto::cipher",
    summary: "AES key handling + CBC / CTR block-cipher modes.",
    items: &[
        StdItem {
            name: "AesKey",
            kind: StdItemKind::Type,
            doc: "Validated key bytes for the chosen size.",
        },
        StdItem {
            name: "AesKeySize",
            kind: StdItemKind::Type,
            doc: "Aes128 / Aes192 / Aes256.",
        },
    ],
};
