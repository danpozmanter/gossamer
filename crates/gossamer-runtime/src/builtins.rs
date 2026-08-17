//! Pure shared builtin helpers.
//!
//! Every helper here is called by **both** the tree-walking
//! interpreter and (eventually) the native backend's runtime
//! stubs. Putting the canonical implementation in one place stops
//! the interpreter from carrying interpreter-specific logic for
//! things that ought to behave the same in compiled code.
//!
//! Functions here are **pure** - they take Rust values and return
//! `String` / primitive outputs. Anything that needs heap
//! allocation or I/O belongs in a later slice that wires the
//! internal ABI through Cranelift.

#![forbid(unsafe_code)]

/// Canonical decimal rendering of a 64-bit signed integer, used
/// wherever Gossamer programs observe an `i64` as text - `println`,
/// `format!("{n}")`, `to_string`, assertion diffs, etc.
#[must_use]
pub fn format_int(n: i64) -> String {
    format!("{n}")
}

/// Canonical decimal rendering of a 64-bit unsigned integer. A slot holds the
/// value's bits, so one at or above `i64::MAX` reads as its own decimal here
/// rather than the negative [`format_int`] would spell.
#[must_use]
pub fn format_uint(n: u64) -> String {
    format!("{n}")
}

/// Canonical rendering of a 64-bit float. Matches Rust's `{f}`
/// format - the interpreter and native backend must not diverge on
/// NaN / infinity / negative-zero output, so the single
/// implementation lives here.
#[must_use]
pub fn format_float(f: f64) -> String {
    format!("{f}")
}

/// Canonical `{:?}` rendering of a float: always recoverable as a float, so
/// an integral value keeps a `.0` and a magnitude outside the plain-decimal
/// window switches to exponent form. Mirrors Rust's `Debug for f64`, which
/// prints decimals with at least one fractional digit for `0` and for
/// `1e-4 <= |x| < 1e16`, and exponent form otherwise.
#[must_use]
pub fn format_float_debug(f: f64) -> String {
    format!("{f:?}")
}

/// Canonical rendering of a boolean: `"true"` / `"false"`. The
/// constant is shared so both paths format the value identically -
/// subtle case differences would otherwise cause parity-harness
/// divergences.
#[must_use]
pub const fn format_bool(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

/// Canonical rendering of the unit value. Hard-coded to `"()"`.
#[must_use]
pub const fn format_unit() -> &'static str {
    "()"
}

/// Canonical prefix for a runtime-error diagnostic. The interpreter
/// already uses this format via `RuntimeError`'s `Display` impl;
/// the native backend's future runtime-panic helper must emit the
/// identical prefix so `cargo test -p gossamer-cli --test parity`
/// sees byte-identical stderr in both paths.
///
/// Callers compose the full message as
/// `format!("{}{}\n", runtime_error_prefix("GX0005"), detail)`.
#[must_use]
pub fn runtime_error_prefix(code: &str) -> String {
    format!("error[{code}]: ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_float_debug_always_reads_back_as_a_float() {
        assert_eq!(format_float_debug(7.0), "7.0");
        assert_eq!(format_float_debug(-0.5), "-0.5");
        assert_eq!(format_float_debug(-0.0), "-0.0");
        assert_eq!(format_float_debug(1e300), "1e300");
        assert_eq!(format_float_debug(1e-10), "1e-10");
        assert_eq!(format_float_debug(f64::NAN), "NaN");
        assert_eq!(format_float_debug(f64::INFINITY), "inf");
        // Display keeps the bare integral spelling.
        assert_eq!(format_float(7.0), "7");
    }

    #[test]
    fn format_int_matches_rust_display_for_extremes() {
        assert_eq!(format_int(0), "0");
        assert_eq!(format_int(i64::MAX), "9223372036854775807");
        assert_eq!(format_int(i64::MIN), "-9223372036854775808");
    }

    #[test]
    fn format_float_preserves_nan_and_infinity_spelling() {
        assert_eq!(format_float(f64::INFINITY), "inf");
        assert_eq!(format_float(f64::NEG_INFINITY), "-inf");
        assert!(format_float(f64::NAN).contains("NaN"));
        assert_eq!(format_float(-0.0), "-0");
    }

    #[test]
    fn format_bool_returns_static_lowercase() {
        assert_eq!(format_bool(true), "true");
        assert_eq!(format_bool(false), "false");
    }

    #[test]
    fn runtime_error_prefix_is_code_bracket_colon_space() {
        assert_eq!(runtime_error_prefix("GX0001"), "error[GX0001]: ");
    }
}
