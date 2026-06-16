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

// Compatibility aliases - SKILL.md and Go's `strconv` use these
// shorter names. The canonical entry points are `parse_i64` /
// `format_i64` etc.; these forward.

/// Alias for [`parse_i64`].
pub fn parse_int(text: &str) -> Result<i64, ParseError> {
    parse_i64(text)
}

/// Alias for [`parse_i64`] - Go-style spelling.
pub fn atoi(text: &str) -> Result<i64, ParseError> {
    parse_i64(text)
}

/// Alias for [`parse_f64`].
pub fn parse_float(text: &str) -> Result<f64, ParseError> {
    parse_f64(text)
}

/// Alias for [`format_i64`].
#[must_use]
pub fn format_int(value: i64) -> String {
    format_i64(value)
}

/// Alias for [`format_i64`] - Go-style spelling.
#[must_use]
pub fn itoa(value: i64) -> String {
    format_i64(value)
}

/// Alias for [`format_f64`].
#[must_use]
pub fn format_float(value: f64) -> String {
    format_f64(value)
}

/// Parses an `i64` in `base` (2..=36), like Go's `strconv.ParseInt(s, base, 64)`.
pub fn parse_i64_radix(text: &str, base: u32) -> Result<i64, ParseError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(ParseError::Empty);
    }
    if !(2..=36).contains(&base) {
        return Err(ParseError::Invalid(format!("invalid base {base}")));
    }
    i64::from_str_radix(trimmed, base).map_err(|err| classify(err, trimmed))
}

/// Renders an `i64` in `base` (2..=36), like Go's `strconv.FormatInt(i, base)`.
/// Out-of-range bases fall back to decimal. Digits a-z are lowercase.
#[must_use]
pub fn format_i64_radix(value: i64, base: u32) -> String {
    if !(2..=36).contains(&base) {
        return format_i64(value);
    }
    if value == 0 {
        return "0".to_string();
    }
    let negative = value < 0;
    // i128 widening so `i64::MIN` negates without overflow.
    let mut n = i128::from(value).unsigned_abs();
    let radix = u128::from(base);
    let mut digits = Vec::new();
    while n > 0 {
        let d = (n % radix) as u32;
        digits.push(std::char::from_digit(d, base).unwrap_or('0'));
        n /= radix;
    }
    if negative {
        digits.push('-');
    }
    digits.iter().rev().collect()
}

/// Wraps `s` in double quotes, escaping `"`, `\`, and control characters,
/// producing a string that [`unquote`] reverses exactly.
#[must_use]
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:04x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Reverses [`quote`]: strips the surrounding double quotes and unescapes the
/// body. Errors when the input is not a well-formed quoted string.
pub fn unquote(s: &str) -> Result<String, ParseError> {
    if s.len() < 2 || !s.starts_with('"') || !s.ends_with('"') {
        return Err(ParseError::Invalid(s.to_string()));
    }
    let inner = &s[1..s.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('u') => {
                if chars.next() != Some('{') {
                    return Err(ParseError::Invalid(s.to_string()));
                }
                let mut hex = String::new();
                for hc in chars.by_ref() {
                    if hc == '}' {
                        break;
                    }
                    hex.push(hc);
                }
                let code = u32::from_str_radix(&hex, 16)
                    .map_err(|_| ParseError::Invalid(s.to_string()))?;
                out.push(char::from_u32(code).ok_or_else(|| ParseError::Invalid(s.to_string()))?);
            }
            _ => return Err(ParseError::Invalid(s.to_string())),
        }
    }
    Ok(out)
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
