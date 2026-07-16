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

pub const RUNTIME: StdModule = StdModule {
    path: "std::runtime",
    summary: "Goroutine / scheduler introspection and tuning.",
    items: &[
        StdItem {
            name: "collect_cycles",
            kind: StdItemKind::Function,
            doc: "Requests collection of unreachable reference cycles; returns `()`.",
        },
        StdItem {
            name: "cycle_collection_supported",
            kind: StdItemKind::Function,
            doc: "Reports whether this execution tier reclaims unreachable reference cycles.",
        },
        StdItem {
            name: "scheduler_stats_json",
            kind: StdItemKind::Function,
            doc: "Returns a compact JSON snapshot of goroutine scheduler counters.",
        },
        StdItem {
            name: "arena_push",
            kind: StdItemKind::Function,
            doc: "Opens an arena region for bump allocation.",
        },
        StdItem {
            name: "arena_pop",
            kind: StdItemKind::Function,
            doc: "Closes the innermost arena region, freeing its slabs.",
        },
        StdItem {
            name: "set_panic_hook",
            kind: StdItemKind::Function,
            doc: "Installs a hook invoked with the message on panic.",
        },
    ],
};

pub const ERRORS: StdModule = StdModule {
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
            name: "newf",
            kind: StdItemKind::Function,
            doc: "Constructs a fresh error from a format template, e.g. `newf(\"status {}\", code)`.",
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
            name: "join",
            kind: StdItemKind::Function,
            doc: "Joins a list of errors into one; messages are joined with \"; \" (None for an empty list).",
        },
    ],
};

pub const FLAG: StdModule = StdModule {
    path: "std::flag",
    summary: "Batteries-included CLI argument parsing.",
    items: &[
        StdItem {
            name: "Value",
            kind: StdItemKind::Type,
            doc: "Parsed command-line flag value.",
        },
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
        StdItem {
            name: "parse",
            kind: StdItemKind::Function,
            doc: "Parses the default flag set against the given args.",
        },
        StdItem {
            name: "string",
            kind: StdItemKind::Function,
            doc: "Defines a string flag on the default set.",
        },
        StdItem {
            name: "int",
            kind: StdItemKind::Function,
            doc: "Defines an integer flag on the default set.",
        },
        StdItem {
            name: "bool",
            kind: StdItemKind::Function,
            doc: "Defines a boolean flag on the default set.",
        },
        StdItem {
            name: "define",
            kind: StdItemKind::Function,
            doc: "Registers a flag definition on the default set.",
        },
    ],
};

pub const SLOG: StdModule = StdModule {
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

pub const TESTING: StdModule = StdModule {
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
        StdItem {
            name: "check_ok",
            kind: StdItemKind::Function,
            doc: "Asserts a Result is Ok, recording without panicking.",
        },
        StdItem {
            name: "wait_for_scheduler_idle",
            kind: StdItemKind::Function,
            doc: "Waits for the scheduler to become idle within a timeout.",
        },
    ],
};

pub const HTTPTEST: StdModule = StdModule {
    path: "std::httptest",
    summary: "Loopback HTTP fixtures for source integration tests.",
    items: &[StdItem {
        name: "server",
        kind: StdItemKind::Function,
        doc: "server(status, body) -> String: starts an isolated loopback static-response server and returns its http:// base URL. Use http::get or http::Client as the test client. The server is test-process scoped and stops when that process exits.",
    }],
};

pub const IMAGE: StdModule = StdModule {
    path: "std::image",
    summary: "Opaque RGBA8 image handles with PNG and JPEG codecs.",
    items: &[
        StdItem {
            name: "new",
            kind: StdItemKind::Function,
            doc: "Allocates a transparent image handle.",
        },
        StdItem {
            name: "filled",
            kind: StdItemKind::Function,
            doc: "Allocates an image handle filled with a packed 0xRRGGBBAA colour.",
        },
        StdItem {
            name: "decode_base64",
            kind: StdItemKind::Function,
            doc: "Decodes a base64 PNG or JPEG; returns zero for malformed input.",
        },
        StdItem {
            name: "width",
            kind: StdItemKind::Function,
            doc: "Returns an image width in pixels.",
        },
        StdItem {
            name: "height",
            kind: StdItemKind::Function,
            doc: "Returns an image height in pixels.",
        },
        StdItem {
            name: "pixel",
            kind: StdItemKind::Function,
            doc: "Returns packed 0xRRGGBBAA, or -1 outside the image.",
        },
        StdItem {
            name: "set_pixel",
            kind: StdItemKind::Function,
            doc: "Sets a packed 0xRRGGBBAA pixel and reports whether it was in bounds.",
        },
        StdItem {
            name: "encode_png_base64",
            kind: StdItemKind::Function,
            doc: "Encodes an image as lossless base64 PNG.",
        },
        StdItem {
            name: "encode_jpeg_base64",
            kind: StdItemKind::Function,
            doc: "Encodes an image as base64 JPEG at quality 1 through 100.",
        },
    ],
};

pub const TIME: StdModule = StdModule {
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
        StdItem {
            name: "now_ms",
            kind: StdItemKind::Function,
            doc: "Wall-clock milliseconds since the Unix epoch.",
        },
        StdItem {
            name: "now_nanos",
            kind: StdItemKind::Function,
            doc: "Wall-clock nanoseconds since the Unix epoch.",
        },
        StdItem {
            name: "unix_ms",
            kind: StdItemKind::Function,
            doc: "Current Unix time in milliseconds.",
        },
        StdItem {
            name: "monotonic_ms",
            kind: StdItemKind::Function,
            doc: "Monotonic clock reading in milliseconds.",
        },
        StdItem {
            name: "monotonic_nanos",
            kind: StdItemKind::Function,
            doc: "Monotonic clock reading in nanoseconds.",
        },
        StdItem {
            name: "since_ms",
            kind: StdItemKind::Function,
            doc: "Milliseconds elapsed since an earlier monotonic reading.",
        },
    ],
};

pub const PANIC: StdModule = StdModule {
    path: "std::panic",
    summary: "Panic / `catch_unwind` integration.",
    items: &[StdItem {
        name: "panic",
        kind: StdItemKind::Macro,
        doc: "Aborts the current goroutine with a message.",
    }],
};

pub const UUID: StdModule = StdModule {
    path: "std::uuid",
    summary: "UUID v4 (random) and v7 (timestamp-ordered) generation, parse, and normalize.",
    items: &[
        StdItem {
            name: "v4",
            kind: StdItemKind::Function,
            doc: "Generate a fresh random v4 UUID as a canonical hyphenated string.",
        },
        StdItem {
            name: "v7",
            kind: StdItemKind::Function,
            doc: "Generate a fresh v7 (timestamp-ordered) UUID.",
        },
        StdItem {
            name: "is_valid",
            kind: StdItemKind::Function,
            doc: "Return true iff the string parses as a canonical UUID.",
        },
        StdItem {
            name: "normalize",
            kind: StdItemKind::Function,
            doc: "Lowercase canonical UUID form of the input, or empty string on parse failure.",
        },
        StdItem {
            name: "simple",
            kind: StdItemKind::Function,
            doc: "32-character unhyphenated form of the input, or empty string on parse failure.",
        },
    ],
};

pub const VALIDATE: StdModule = StdModule {
    path: "std::validate",
    summary: "Trait-based field validation: implement Validate, collect FieldErrors into Errors.",
    items: &[
        StdItem {
            name: "Validate",
            kind: StdItemKind::Trait,
            doc: "Implement on a struct to declare field-level validation rules.",
        },
        StdItem {
            name: "FieldError",
            kind: StdItemKind::Type,
            doc: "One field-scoped validation failure: dotted path, message, optional code.",
        },
        StdItem {
            name: "Errors",
            kind: StdItemKind::Type,
            doc: "Aggregated FieldError set, indexable by dotted path.",
        },
    ],
};

pub const METRICS: StdModule = StdModule {
    path: "std::metrics",
    summary: "Prometheus-compatible primitives (Counter, Gauge, Histogram) and a Registry rendering the standard text-exposition format.",
    items: &[
        StdItem {
            name: "Counter",
            kind: StdItemKind::Type,
            doc: "Monotonic-increasing u64 counter (lock-free).",
        },
        StdItem {
            name: "Gauge",
            kind: StdItemKind::Type,
            doc: "Set / inc / dec gauge (lock-free).",
        },
        StdItem {
            name: "Histogram",
            kind: StdItemKind::Type,
            doc: "Bucketed observation histogram with sum and count.",
        },
        StdItem {
            name: "Metric",
            kind: StdItemKind::Type,
            doc: "Enum holding any of the three primitives for registry storage.",
        },
        StdItem {
            name: "Registry",
            kind: StdItemKind::Type,
            doc: "Ordered collection of metrics; `render()` emits the Prometheus text-exposition format.",
        },
        StdItem {
            name: "serve_metrics",
            kind: StdItemKind::Function,
            doc: "Mounts a registry on `/metrics` over the existing http server loop.",
        },
    ],
};

pub const TRACE: StdModule = StdModule {
    path: "std::trace",
    summary: "W3C trace-context-compatible distributed tracing. Identifier types, request-scoped SpanContext, process-level Tracer, and OTLP JSON export.",
    items: &[
        StdItem {
            name: "TraceId",
            kind: StdItemKind::Type,
            doc: "128-bit trace identifier (W3C trace-context format).",
        },
        StdItem {
            name: "SpanId",
            kind: StdItemKind::Type,
            doc: "64-bit span identifier.",
        },
        StdItem {
            name: "SpanContext",
            kind: StdItemKind::Type,
            doc: "Request-scoped trace + span pair, propagated through `std::context`.",
        },
        StdItem {
            name: "SpanStatus",
            kind: StdItemKind::Type,
            doc: "Span outcome: Unset / Ok / Error(message).",
        },
        StdItem {
            name: "Span",
            kind: StdItemKind::Type,
            doc: "Active span builder. Attributes, events, status; `end()` finalises and records.",
        },
        StdItem {
            name: "EndedSpan",
            kind: StdItemKind::Type,
            doc: "Finalised span record; `to_otlp_json()` serialises for OTLP/HTTP export.",
        },
        StdItem {
            name: "Tracer",
            kind: StdItemKind::Type,
            doc: "Process-level span sink. `start_span`, `ended_spans`, `set_global`.",
        },
        StdItem {
            name: "SpanGuard",
            kind: StdItemKind::Type,
            doc: "RAII guard returned by `enter_span`; restores the prior active span on drop.",
        },
    ],
};
