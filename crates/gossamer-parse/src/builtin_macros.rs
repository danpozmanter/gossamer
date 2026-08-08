//! Public contract for the parser-recognized macro surface.
//!
//! Gossamer deliberately supports a fixed set of built-in macros. Keeping
//! their names and user-facing help together lets the parser and REPL stay in
//! sync without creating a runtime macro system.

#![forbid(unsafe_code)]

/// One built-in macro exposed by the parser and REPL help.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinMacro {
    /// Source spelling, including the required `!`.
    pub name: &'static str,
    /// Compact invocation form for interactive help.
    pub signature: &'static str,
    /// One-line behavior summary.
    pub doc: &'static str,
}

/// Every macro invocation recognized by Gossamer's parser.
pub const BUILTIN_MACROS: &[BuiltinMacro] = &[
    BuiltinMacro {
        name: "format!",
        signature: "format!(\"template\", values...) -> String",
        doc: "Formats a literal template into an owned String; every explicit argument needs a positional placeholder.",
    },
    BuiltinMacro {
        name: "println!",
        signature: "println!(\"template\", values...)",
        doc: "Formats a literal template to stdout followed by a newline; every explicit argument needs a positional placeholder, and a piped value occupies `$`.",
    },
    BuiltinMacro {
        name: "print!",
        signature: "print!(\"template\", values...)",
        doc: "Formats a literal template to stdout without a newline; every explicit argument needs a positional placeholder, and a piped value occupies `$`.",
    },
    BuiltinMacro {
        name: "eprintln!",
        signature: "eprintln!(\"template\", values...)",
        doc: "Formats a literal template to stderr followed by a newline; every explicit argument needs a positional placeholder, and a piped value occupies `$`.",
    },
    BuiltinMacro {
        name: "eprint!",
        signature: "eprint!(\"template\", values...)",
        doc: "Formats a literal template to stderr without a newline; every explicit argument needs a positional placeholder, and a piped value occupies `$`.",
    },
    BuiltinMacro {
        name: "panic!",
        signature: "panic!(\"template\", values...) -> !",
        doc: "Panics with a formatted literal message; every explicit argument needs a positional placeholder.",
    },
    BuiltinMacro {
        name: "matches!",
        signature: "matches!(expr, pattern) -> bool",
        doc: "Returns whether expr matches pattern.",
    },
    BuiltinMacro {
        name: "todo!",
        signature: "todo!(\"message\"?) -> !",
        doc: "Panics to mark intentionally unfinished code.",
    },
    BuiltinMacro {
        name: "unimplemented!",
        signature: "unimplemented!(\"message\"?) -> !",
        doc: "Panics to mark an unsupported path.",
    },
    BuiltinMacro {
        name: "unreachable!",
        signature: "unreachable!(\"message\"?) -> !",
        doc: "Panics when an impossible path is reached.",
    },
    BuiltinMacro {
        name: "dbg!",
        signature: "dbg!(expr) -> T",
        doc: "Writes the debug rendering of expr to stderr and returns it.",
    },
    BuiltinMacro {
        name: "regex!",
        signature: "regex!(\"pattern\") -> regex::Pattern",
        doc: "Validates a regular-expression literal at build time.",
    },
    BuiltinMacro {
        name: "sql!",
        signature: "sql!(\"query\")",
        doc: "Validates a SQL literal at build time when a driver can validate it.",
    },
    BuiltinMacro {
        name: "codegen!",
        signature: "codegen!(expr)",
        doc: "Splices a comptime String result back into source code.",
    },
];

/// Whether `name` is one of the format-template macros.
#[must_use]
pub fn is_format_macro(name: &str) -> bool {
    matches!(
        name,
        "format" | "println" | "print" | "eprintln" | "eprint" | "panic"
    )
}

/// Whether `name` is one of the macros that desugars to ordinary AST.
#[must_use]
pub fn is_desugar_macro(name: &str) -> bool {
    matches!(
        name,
        "matches" | "todo" | "unimplemented" | "unreachable" | "dbg"
    )
}

/// Whether `name` is a build-time validation or source-generation macro.
#[must_use]
pub fn is_comptime_macro(name: &str) -> bool {
    matches!(name, "regex" | "sql" | "codegen")
}
