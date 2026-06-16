// `std::mime` - RFC 2045 media type parsing + extension lookup.
//
// Surface (simple scalar-only API for cross-tier wiring):
//   - `parse(s)` -> "type/subtype" if valid, else ""
//   - `top(s)` -> top-level type ("text", "image", etc.) or ""
//   - `sub(s)` -> subtype ("html", "png", ...) or ""
//   - `charset(s)` -> charset param ("utf-8") or ""
//   - `boundary(s)` -> boundary param or ""
//   - `param(s, key)` -> arbitrary param value or ""
//   - `type_by_extension(ext)` -> canonical type for ".png" (no
//     leading-dot version also accepted); "" if unknown.
//   - `extension_by_type(t)` -> canonical extension (no leading dot)
//     for "text/html"; "" if unknown.

#![forbid(unsafe_code)]

use mime::Mime;

fn parse_mime(s: &str) -> Option<Mime> {
    s.parse::<Mime>().ok()
}

/// Canonical "type/subtype" form if `s` parses; "" otherwise.
#[must_use]
pub fn parse(s: &str) -> String {
    match parse_mime(s) {
        Some(m) => format!("{}/{}", m.type_(), m.subtype()),
        None => String::new(),
    }
}

/// Top-level type ("text", "image", ...) or "".
#[must_use]
pub fn top(s: &str) -> String {
    parse_mime(s).map_or_else(String::new, |m| m.type_().to_string())
}

/// Subtype ("html", "png", ...) or "".
#[must_use]
pub fn sub(s: &str) -> String {
    parse_mime(s).map_or_else(String::new, |m| m.subtype().to_string())
}

/// Charset parameter ("utf-8") or "".
#[must_use]
pub fn charset(s: &str) -> String {
    parse_mime(s)
        .and_then(|m| m.get_param("charset").map(|v| v.to_string()))
        .unwrap_or_default()
}

/// Boundary parameter (for multipart) or "".
#[must_use]
pub fn boundary(s: &str) -> String {
    parse_mime(s)
        .and_then(|m| m.get_param("boundary").map(|v| v.to_string()))
        .unwrap_or_default()
}

/// Arbitrary parameter value, or "".
#[must_use]
pub fn param(s: &str, key: &str) -> String {
    parse_mime(s)
        .and_then(|m| m.get_param(key).map(|v| v.to_string()))
        .unwrap_or_default()
}

/// Canonical media type for the given filename extension. Accepts
/// extensions with or without a leading dot. Returns "" if unknown.
#[must_use]
pub fn type_by_extension(ext: &str) -> String {
    let trimmed = ext.strip_prefix('.').unwrap_or(ext);
    if trimmed.is_empty() {
        return String::new();
    }
    mime_guess::from_ext(trimmed)
        .first()
        .map(|m| m.essence_str().to_string())
        .unwrap_or_default()
}

/// Canonical extension (no leading dot) for the given media type.
#[must_use]
pub fn extension_by_type(t: &str) -> String {
    let Some(m) = parse_mime(t) else {
        return String::new();
    };
    let essence = format!("{}/{}", m.type_(), m.subtype());
    mime_guess::get_mime_extensions_str(&essence)
        .and_then(|exts| exts.first())
        .map(ToString::to_string)
        .unwrap_or_default()
}

/// `true` iff `s` parses as a valid media type.
#[must_use]
pub fn is_valid(s: &str) -> bool {
    parse_mime(s).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_canonical() {
        assert_eq!(parse("text/html; charset=utf-8"), "text/html");
        assert_eq!(parse("application/json"), "application/json");
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(parse(""), "");
        assert_eq!(parse("not a mime"), "");
    }

    #[test]
    fn top_and_sub() {
        assert_eq!(top("text/html"), "text");
        assert_eq!(sub("text/html"), "html");
    }

    #[test]
    fn charset_param() {
        assert_eq!(charset("text/html; charset=utf-8"), "utf-8");
        assert_eq!(charset("text/plain"), "");
    }

    #[test]
    fn boundary_param() {
        let s = "multipart/form-data; boundary=----abc123";
        assert_eq!(boundary(s), "----abc123");
    }

    #[test]
    fn type_by_extension_with_dot() {
        assert_eq!(type_by_extension(".html"), "text/html");
        assert_eq!(type_by_extension("html"), "text/html");
        assert_eq!(type_by_extension("png"), "image/png");
    }

    #[test]
    fn type_by_extension_unknown() {
        assert_eq!(type_by_extension(".zzzzz"), "");
    }

    #[test]
    fn extension_by_type_roundtrip() {
        // Not all media types round-trip cleanly (e.g. several
        // extensions map to text/plain) - just verify a stable
        // entry is returned.
        let e = extension_by_type("image/png");
        assert!(!e.is_empty());
    }

    #[test]
    fn is_valid_basic() {
        assert!(is_valid("text/html"));
        assert!(is_valid("application/json"));
        assert!(!is_valid(""));
        assert!(!is_valid("text"));
    }
}
