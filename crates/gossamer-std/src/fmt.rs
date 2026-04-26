//! Runtime support for `std::fmt`.
//! These helpers are the concrete implementations behind the
//! macro-shaped entries in [`crate::manifest::ALL_MODULES`].
//! `gossamer-interp` and the eventual native runtime call into them
//! when a Gossamer program invokes `println!`, `format!`, etc.

#![forbid(unsafe_code)]

use std::fmt::Write;

/// Joins every argument with single spaces, mirroring the v1 default
/// `println` semantics used by the interpreter.
#[must_use]
pub fn join_with_spaces<'a, I>(parts: I) -> String
where
    I: IntoIterator<Item = &'a str>,
{
    let mut out = String::new();
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(part);
    }
    out
}

/// Format an integer using the canonical decimal Display rendering.
#[must_use]
pub fn format_int(value: i64) -> String {
    let mut out = String::new();
    let _ = write!(out, "{value}");
    out
}

/// Format a float using the canonical Rust `{}` Display rendering.
#[must_use]
pub fn format_float(value: f64) -> String {
    let mut out = String::new();
    let _ = write!(out, "{value}");
    out
}

/// Format a boolean as `"true"` / `"false"`.
#[must_use]
pub const fn format_bool(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}
