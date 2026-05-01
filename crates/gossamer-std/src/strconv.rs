//! Runtime support for `std::strconv`.

#![forbid(unsafe_code)]

use std::fmt::Write;

use thiserror::Error;

/// Parse errors surfaced by `parse_*` functions.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    /// The input was empty.
    #[error("empty input")]
    Empty,
    /// The input contained an invalid character.
    #[error("invalid input: {0:?}")]
    Invalid(String),
    /// The value would overflow the target type.
    #[error("overflow parsing {0:?}")]
    Overflow(String),
}

/// Parses a decimal `i64`.
pub fn parse_i64(text: &str) -> Result<i64, ParseError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(ParseError::Empty);
    }
    trimmed.parse::<i64>().map_err(|err| classify(err, trimmed))
}

/// Parses a decimal `u64`.
pub fn parse_u64(text: &str) -> Result<u64, ParseError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(ParseError::Empty);
    }
    trimmed.parse::<u64>().map_err(|err| classify(err, trimmed))
}

/// Parses a decimal `f64`.
pub fn parse_f64(text: &str) -> Result<f64, ParseError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(ParseError::Empty);
    }
    trimmed
        .parse::<f64>()
        .map_err(|_| ParseError::Invalid(trimmed.to_string()))
}

/// Parses `"true"` / `"false"` (case-sensitive) into a bool.
pub fn parse_bool(text: &str) -> Result<bool, ParseError> {
    match text {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(ParseError::Invalid(other.to_string())),
    }
}

/// Renders an `i64` as a decimal string.
#[must_use]
pub fn format_i64(value: i64) -> String {
    let mut out = String::new();
    let _ = write!(out, "{value}");
    out
}

/// Renders an `f64` as a decimal string using the default Display
/// rendering.
#[must_use]
pub fn format_f64(value: f64) -> String {
    let mut out = String::new();
    let _ = write!(out, "{value}");
    out
}

fn classify(err: std::num::ParseIntError, text: &str) -> ParseError {
    use std::num::IntErrorKind;
    match err.kind() {
        IntErrorKind::Empty => ParseError::Empty,
        IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => {
            ParseError::Overflow(text.to_string())
        }
        _ => ParseError::Invalid(text.to_string()),
    }
}
