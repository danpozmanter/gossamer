//! Static manifest of every registered stdlib module.
//! Each stdlib milestone extends this table with
//! the modules it adds. Entries are listed in phase-introduction order
//! so a `gos doc` walk renders modules in the same sequence as the
//! implementation plan.

#![forbid(unsafe_code)]

use crate::registry::{StdItem, StdItemKind, StdModule};

/// Single source of truth for the stdlib's public surface.
pub const ALL_MODULES: &[StdModule] = &[
    FMT,
    IO,
    OS,
    OS_EXEC,
    OS_SIGNAL,
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
        StdItem {
            name: "walk",
            kind: StdItemKind::Function,
            doc: "Recursive directory walk; returns Result<[String], String> of every descendant path. Aliases fs::walk_dir.",
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
    summary: "Filesystem walking and mutation helpers.",
    items: &[
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
            name: "create_dir_all",
            kind: StdItemKind::Function,
            doc: "Creates a path and any missing ancestors.",
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

const ENCODING_BINARY: StdModule = StdModule {
    path: "std::encoding::binary",
    summary: "Big-endian / little-endian integer packing.",
    items: &[
        StdItem {
            name: "put_u16_be",
            kind: StdItemKind::Function,
            doc: "Writes a big-endian u16.",
        },
        StdItem {
            name: "put_u32_be",
            kind: StdItemKind::Function,
            doc: "Writes a big-endian u32.",
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
    summary: "Operating-system primitives: filesystem, env, process.",
    items: &[
        StdItem {
            name: "args",
            kind: StdItemKind::Function,
            doc: "Returns the program's command-line arguments.",
        },
        StdItem {
            name: "env",
            kind: StdItemKind::Function,
            doc: "Returns the value of an environment variable.",
        },
        StdItem {
            name: "set_env",
            kind: StdItemKind::Function,
            doc: "Sets an environment variable in the current process.",
        },
        StdItem {
            name: "exit",
            kind: StdItemKind::Function,
            doc: "Exits the process with the given status code.",
        },
        StdItem {
            name: "open",
            kind: StdItemKind::Function,
            doc: "Opens a file for reading.",
        },
        StdItem {
            name: "create",
            kind: StdItemKind::Function,
            doc: "Creates or truncates a file for writing.",
        },
        StdItem {
            name: "read_file",
            kind: StdItemKind::Function,
            doc: "Reads an entire file into memory.",
        },
        StdItem {
            name: "write_file",
            kind: StdItemKind::Function,
            doc: "Writes the given bytes to a file, creating it if needed.",
        },
        StdItem {
            name: "remove_file",
            kind: StdItemKind::Function,
            doc: "Removes a file from the filesystem.",
        },
        StdItem {
            name: "rename",
            kind: StdItemKind::Function,
            doc: "Renames a file or directory.",
        },
        StdItem {
            name: "exists",
            kind: StdItemKind::Function,
            doc: "Returns whether a path exists.",
        },
        StdItem {
            name: "mkdir",
            kind: StdItemKind::Function,
            doc: "Creates a single directory.",
        },
        StdItem {
            name: "mkdir_all",
            kind: StdItemKind::Function,
            doc: "Creates a directory and any required parents.",
        },
        StdItem {
            name: "read_dir",
            kind: StdItemKind::Function,
            doc: "Iterates the entries of a directory.",
        },
        StdItem {
            name: "File",
            kind: StdItemKind::Type,
            doc: "Open file handle supporting read/write/seek/close.",
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
            name: "to_lowercase",
            kind: StdItemKind::Function,
            doc: "Lowercases every character.",
        },
        StdItem {
            name: "to_uppercase",
            kind: StdItemKind::Function,
            doc: "Uppercases every character.",
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
    summary: "HTTP/1.1 client and server.",
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
