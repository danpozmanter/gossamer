//! RFC 6265 HTTP cookie parsing and building.
//!
//! Two parser entry points cover the wire shapes:
//!
//! - `parse_cookie_header` reads the client-sent `Cookie:` header
//!   (`name1=value1; name2=value2`) and is lenient — malformed pairs
//!   are skipped silently.
//! - `parse_set_cookie` reads a server-sent `Set-Cookie:` header
//!   (`name=value; Path=/; HttpOnly`) and is strict on the name=value
//!   pair, lenient on unknown attributes (they are dropped).
//!
//! Construction goes through `Cookie::builder` for the attribute-
//! rich path, or `Cookie::new` for the bare `name=value` shape.
//! `Cookie::to_header_value` renders a value suitable for a
//! `Set-Cookie:` header.

#![forbid(unsafe_code)]
#![allow(
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::needless_pass_by_value
)]

use crate::errors::Error;

/// `SameSite` attribute values per RFC 6265bis.
///
/// `None` requires that the cookie also set `Secure=true` per the
/// spec; this module documents the rule but does not enforce it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSite {
    /// Cookie is sent only on same-site requests.
    Strict,
    /// Cookie is sent on same-site requests and top-level cross-site navigations.
    Lax,
    /// Cookie is sent on all requests; requires `Secure=true`.
    None,
}

impl SameSite {
    /// Returns the canonical attribute spelling (`Strict`, `Lax`, `None`).
    pub fn as_str(self) -> &'static str {
        match self {
            SameSite::Strict => "Strict",
            SameSite::Lax => "Lax",
            SameSite::None => "None",
        }
    }

    /// Parses a `SameSite` attribute value case-insensitively.
    pub fn parse(s: &str) -> Option<SameSite> {
        let t = s.trim();
        if t.eq_ignore_ascii_case("strict") {
            Some(SameSite::Strict)
        } else if t.eq_ignore_ascii_case("lax") {
            Some(SameSite::Lax)
        } else if t.eq_ignore_ascii_case("none") {
            Some(SameSite::None)
        } else {
            None
        }
    }
}

/// A parsed or constructed HTTP cookie.
///
/// Field shapes follow RFC 6265 §4.1. `max_age` is in seconds;
/// a negative value instructs the user agent to delete the cookie.
/// `expires` is held as the raw RFC 1123 date string for round-trip
/// fidelity — interpretation is left to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    /// Cookie name (the token before `=`).
    pub name: String,
    /// Cookie value (the bytes after `=`).
    pub value: String,
    /// `Domain` attribute, if any.
    pub domain: Option<String>,
    /// `Path` attribute, if any.
    pub path: Option<String>,
    /// `Max-Age` attribute in seconds; negative deletes the cookie.
    pub max_age: Option<i64>,
    /// `Expires` attribute as an RFC 1123 date string.
    pub expires: Option<String>,
    /// `HttpOnly` flag.
    pub http_only: bool,
    /// `Secure` flag.
    pub secure: bool,
    /// `SameSite` attribute.
    pub same_site: Option<SameSite>,
}

impl Cookie {
    /// Constructs a bare `name=value` cookie with no attributes set.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Cookie {
            name: name.into(),
            value: value.into(),
            domain: None,
            path: None,
            max_age: None,
            expires: None,
            http_only: false,
            secure: false,
            same_site: None,
        }
    }

    /// Opens a builder for incremental attribute configuration.
    pub fn builder(name: impl Into<String>, value: impl Into<String>) -> CookieBuilder {
        CookieBuilder {
            inner: Cookie::new(name, value),
        }
    }

    /// Serializes the cookie to a `Set-Cookie` header value.
    ///
    /// Values containing cookie-octet-illegal bytes (whitespace,
    /// `,`, `;`, `\`, `"`, control chars) are wrapped in a
    /// quoted-string per RFC 6265 §4.1.1. Inner `"` and `\` are
    /// backslash-escaped.
    pub fn to_header_value(&self) -> String {
        let mut out = String::with_capacity(self.name.len() + self.value.len() + 8);
        out.push_str(&sanitize_cookie_name(&self.name));
        out.push('=');
        out.push_str(&encode_value(&self.value));

        if let Some(d) = &self.domain {
            out.push_str("; Domain=");
            out.push_str(d);
        }
        if let Some(p) = &self.path {
            out.push_str("; Path=");
            out.push_str(p);
        }
        if let Some(m) = self.max_age {
            out.push_str("; Max-Age=");
            out.push_str(&m.to_string());
        }
        if let Some(e) = &self.expires {
            out.push_str("; Expires=");
            out.push_str(e);
        }
        if self.http_only {
            out.push_str("; HttpOnly");
        }
        if self.secure {
            out.push_str("; Secure");
        }
        if let Some(s) = self.same_site {
            out.push_str("; SameSite=");
            out.push_str(s.as_str());
        }
        out
    }
}

/// Incremental builder for [`Cookie`].
///
/// Methods consume and return `self` so the chain reads top-down.
#[derive(Debug, Clone)]
pub struct CookieBuilder {
    inner: Cookie,
}

impl CookieBuilder {
    /// Sets the `Domain` attribute.
    #[must_use]
    pub fn domain(mut self, d: impl Into<String>) -> Self {
        self.inner.domain = Some(d.into());
        self
    }

    /// Sets the `Path` attribute.
    #[must_use]
    pub fn path(mut self, p: impl Into<String>) -> Self {
        self.inner.path = Some(p.into());
        self
    }

    /// Sets the `Max-Age` attribute in seconds; negative deletes.
    #[must_use]
    pub fn max_age(mut self, secs: i64) -> Self {
        self.inner.max_age = Some(secs);
        self
    }

    /// Sets the `Expires` attribute as an RFC 1123 date string.
    #[must_use]
    pub fn expires(mut self, rfc1123: impl Into<String>) -> Self {
        self.inner.expires = Some(rfc1123.into());
        self
    }

    /// Sets the `HttpOnly` flag.
    #[must_use]
    pub fn http_only(mut self, on: bool) -> Self {
        self.inner.http_only = on;
        self
    }

    /// Sets the `Secure` flag.
    #[must_use]
    pub fn secure(mut self, on: bool) -> Self {
        self.inner.secure = on;
        self
    }

    /// Sets the `SameSite` attribute.
    #[must_use]
    pub fn same_site(mut self, s: SameSite) -> Self {
        self.inner.same_site = Some(s);
        self
    }

    /// Finalizes the builder into a [`Cookie`].
    pub fn build(self) -> Cookie {
        self.inner
    }
}

/// Parses a `Cookie:` request header into `(name, value)` pairs.
///
/// Lenient — malformed pairs (missing `=`, empty name) are skipped
/// rather than reported. Values that are wrapped in `"..."` are
/// unquoted. Order is preserved.
pub fn parse_cookie_header(header: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in header.split(';') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some(eq) = trimmed.find('=') else {
            continue;
        };
        let name = trimmed[..eq].trim();
        let value = trimmed[eq + 1..].trim();
        if name.is_empty() {
            continue;
        }
        let value = unquote(value);
        out.push((name.to_string(), value));
    }
    out
}

/// Parses a `Set-Cookie:` response header into a [`Cookie`].
///
/// The leading `name=value` pair is required; unknown attributes
/// are ignored. Attribute names are matched case-insensitively.
pub fn parse_set_cookie(header: &str) -> Result<Cookie, Error> {
    let mut parts = header.split(';');
    let first = parts
        .next()
        .ok_or_else(|| Error::new("set-cookie: empty header"))?
        .trim();
    let Some(eq) = first.find('=') else {
        return Err(Error::new("set-cookie: missing '=' in first pair"));
    };
    let name = first[..eq].trim();
    if name.is_empty() {
        return Err(Error::new("set-cookie: empty cookie name"));
    }
    let value = unquote(first[eq + 1..].trim());
    let mut cookie = Cookie::new(name.to_string(), value);

    for attr in parts {
        let attr = attr.trim();
        if attr.is_empty() {
            continue;
        }
        let (key, val) = match attr.find('=') {
            Some(i) => (attr[..i].trim(), Some(attr[i + 1..].trim())),
            None => (attr, None),
        };

        if key.eq_ignore_ascii_case("Domain") {
            if let Some(v) = val {
                cookie.domain = Some(v.to_string());
            }
        } else if key.eq_ignore_ascii_case("Path") {
            if let Some(v) = val {
                cookie.path = Some(v.to_string());
            }
        } else if key.eq_ignore_ascii_case("Max-Age") {
            if let Some(v) = val {
                let n: i64 = v
                    .parse()
                    .map_err(|_| Error::new(format!("set-cookie: invalid Max-Age value '{v}'")))?;
                cookie.max_age = Some(n);
            }
        } else if key.eq_ignore_ascii_case("Expires") {
            if let Some(v) = val {
                cookie.expires = Some(v.to_string());
            }
        } else if key.eq_ignore_ascii_case("HttpOnly") {
            cookie.http_only = true;
        } else if key.eq_ignore_ascii_case("Secure") {
            cookie.secure = true;
        } else if key.eq_ignore_ascii_case("SameSite") {
            if let Some(v) = val {
                cookie.same_site = SameSite::parse(v);
            }
        }
        // Unknown attributes pass through silently.
    }

    Ok(cookie)
}

// Renders `value` as a header-safe cookie value. Mirrors Go's
// `net/http` sanitizeCookieValue: keep printable ASCII except `"`,
// `;`, `\` and drop every other byte (CR, LF, NUL, DEL, high bytes),
// then wrap in DQUOTE when the result contains a space or comma. The
// dropped bytes are exactly those that would split the `Set-Cookie`
// header or inject cookie attributes, so a value built from untrusted
// input can never break out of its slot.
fn encode_value(value: &str) -> String {
    let sanitized: String = value
        .bytes()
        .filter(|&b| (0x20..=0x7e).contains(&b) && b != b'"' && b != b';' && b != b'\\')
        .map(char::from)
        .collect();
    if sanitized.bytes().any(|b| b == b' ' || b == b',') {
        format!("\"{sanitized}\"")
    } else {
        sanitized
    }
}

// Strips CR, LF, and NUL from a cookie name so a name built from
// untrusted input cannot inject a new header line. (A conformant name
// is a token; this is the minimal split-prevention guard.)
fn sanitize_cookie_name(name: &str) -> String {
    name.chars()
        .filter(|&c| c != '\r' && c != '\n' && c != '\0')
        .collect()
}

// Strips a surrounding pair of `"` if present and unescapes the
// `\\` / `\"` pair sequences produced by [`encode_value`].
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        let inner = &value[1..value.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut escaped = false;
        for ch in inner.chars() {
            if escaped {
                out.push(ch);
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else {
                out.push(ch);
            }
        }
        if escaped {
            out.push('\\');
        }
        out
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_only_name_and_value() {
        let c = Cookie::new("session", "abc123");
        assert_eq!(c.name, "session");
        assert_eq!(c.value, "abc123");
        assert!(c.domain.is_none());
        assert!(c.path.is_none());
        assert!(!c.http_only);
        assert!(!c.secure);
        assert!(c.same_site.is_none());
    }

    #[test]
    fn builder_round_trip_through_parse_set_cookie() {
        let original = Cookie::builder("session", "abc123")
            .domain("example.com")
            .path("/")
            .max_age(3600)
            .http_only(true)
            .secure(true)
            .same_site(SameSite::Lax)
            .build();

        let header = original.to_header_value();
        let parsed = parse_set_cookie(&header).expect("parse succeeds");

        assert_eq!(parsed.name, "session");
        assert_eq!(parsed.value, "abc123");
        assert_eq!(parsed.domain.as_deref(), Some("example.com"));
        assert_eq!(parsed.path.as_deref(), Some("/"));
        assert_eq!(parsed.max_age, Some(3600));
        assert!(parsed.http_only);
        assert!(parsed.secure);
        assert_eq!(parsed.same_site, Some(SameSite::Lax));
    }

    #[test]
    fn to_header_value_omits_unset_attributes() {
        let header = Cookie::new("k", "v").to_header_value();
        assert_eq!(header, "k=v");
    }

    #[test]
    fn to_header_value_quotes_values_with_special_chars() {
        // Values containing whitespace or other non-token chars are
        // wrapped in a quoted-string per RFC 6265 §4.1.1. The round
        // trip back through parse_set_cookie is intentionally not
        // asserted here: parse_set_cookie splits attributes on `;`
        // outside of quoted strings, and round-tripping embedded
        // semicolons inside a quoted value is a known gap (cookie
        // attribute parsing does not preserve quote state across
        // splits). Whitespace-only specials do round-trip; the
        // tighter test above (`to_header_value_escapes_quote_and_backslash`)
        // exercises the escape path.
        let c = Cookie::new("k", "hello world");
        let header = c.to_header_value();
        assert!(header.starts_with("k=\""));
        assert!(header.contains("hello world"));
        let parsed = parse_set_cookie(&header).expect("parse succeeds");
        assert_eq!(parsed.value, "hello world");
    }

    #[test]
    fn to_header_value_drops_quote_and_backslash() {
        // `"` and `\` are not valid cookie-octets and cannot be safely
        // represented, so they are dropped (matching Go's net/http).
        let c = Cookie::new("k", "a\"b\\c");
        let header = c.to_header_value();
        assert_eq!(header, "k=abc");
    }

    #[test]
    fn to_header_value_strips_crlf_from_value() {
        // An app setting a cookie from untrusted input must not be able
        // to inject a second header line (HTTP response splitting). The
        // CR/LF are dropped, so the payload stays a single header line.
        let c = Cookie::new("lang", "en\r\nSet-Cookie: pwned=1");
        let header = c.to_header_value();
        assert!(
            !header.contains('\r') && !header.contains('\n'),
            "got {header:?}"
        );
        assert_eq!(header.lines().count(), 1, "must stay one line: {header:?}");
    }

    #[test]
    fn to_header_value_drops_semicolon_to_block_attribute_injection() {
        let c = Cookie::new("k", "v; Domain=evil.example; Path=/admin");
        let header = c.to_header_value();
        // No raw `;` survives, so a browser cannot split the value into
        // attacker-chosen cookie attributes.
        assert!(!header.contains(';'), "got {header:?}");
    }

    #[test]
    fn to_header_value_strips_crlf_from_name() {
        let c = Cookie::new("a\r\nSet-Cookie: x", "1");
        let header = c.to_header_value();
        assert!(
            !header.contains('\r') && !header.contains('\n'),
            "got {header:?}"
        );
    }

    #[test]
    fn parse_cookie_header_splits_multiple_pairs() {
        let pairs = parse_cookie_header("a=1; b=2; c=3");
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0], ("a".to_string(), "1".to_string()));
        assert_eq!(pairs[1], ("b".to_string(), "2".to_string()));
        assert_eq!(pairs[2], ("c".to_string(), "3".to_string()));
    }

    #[test]
    fn parse_cookie_header_is_lenient() {
        let pairs = parse_cookie_header("a=1; ; broken; =bad; b=2");
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0, "a");
        assert_eq!(pairs[1].0, "b");
    }

    #[test]
    fn parse_cookie_header_unquotes_values() {
        let pairs = parse_cookie_header("session=\"hello world\"; other=plain");
        assert_eq!(pairs[0].1, "hello world");
        assert_eq!(pairs[1].1, "plain");
    }

    #[test]
    fn parse_set_cookie_with_all_attributes() {
        let header = "id=42; Domain=example.com; Path=/; \
                      Max-Age=120; Expires=Wed, 09 Jun 2027 10:18:14 GMT; \
                      HttpOnly; Secure; SameSite=Strict";
        let c = parse_set_cookie(header).expect("parse");
        assert_eq!(c.name, "id");
        assert_eq!(c.value, "42");
        assert_eq!(c.domain.as_deref(), Some("example.com"));
        assert_eq!(c.path.as_deref(), Some("/"));
        assert_eq!(c.max_age, Some(120));
        assert_eq!(c.expires.as_deref(), Some("Wed, 09 Jun 2027 10:18:14 GMT"));
        assert!(c.http_only);
        assert!(c.secure);
        assert_eq!(c.same_site, Some(SameSite::Strict));
    }

    #[test]
    fn parse_set_cookie_attribute_names_are_case_insensitive() {
        let header = "x=y; path=/api; HTTPONLY; sEcUrE; samesite=lax";
        let c = parse_set_cookie(header).expect("parse");
        assert_eq!(c.path.as_deref(), Some("/api"));
        assert!(c.http_only);
        assert!(c.secure);
        assert_eq!(c.same_site, Some(SameSite::Lax));
    }

    #[test]
    fn parse_set_cookie_same_site_case_insensitive_values() {
        for (raw, want) in [
            ("STRICT", SameSite::Strict),
            ("strict", SameSite::Strict),
            ("Lax", SameSite::Lax),
            ("LAX", SameSite::Lax),
            ("none", SameSite::None),
            ("None", SameSite::None),
        ] {
            let header = format!("k=v; SameSite={raw}");
            let c = parse_set_cookie(&header).expect("parse");
            assert_eq!(c.same_site, Some(want), "raw={raw}");
        }
    }

    #[test]
    fn parse_set_cookie_negative_max_age() {
        let c = parse_set_cookie("k=v; Max-Age=-1").expect("parse");
        assert_eq!(c.max_age, Some(-1));
    }

    #[test]
    fn parse_set_cookie_invalid_max_age_errors() {
        let err = parse_set_cookie("k=v; Max-Age=not-a-number").unwrap_err();
        assert!(err.message().contains("Max-Age"));
    }

    #[test]
    fn parse_set_cookie_missing_equals_errors() {
        let err = parse_set_cookie("just-a-name").unwrap_err();
        assert!(err.message().contains("missing '='"));
    }

    #[test]
    fn parse_set_cookie_empty_name_errors() {
        let err = parse_set_cookie("=value").unwrap_err();
        assert!(err.message().contains("empty cookie name"));
    }

    #[test]
    fn parse_set_cookie_unknown_attributes_pass_through() {
        let c = parse_set_cookie("k=v; Priority=High; Partitioned; Path=/").expect("parse");
        assert_eq!(c.name, "k");
        assert_eq!(c.value, "v");
        assert_eq!(c.path.as_deref(), Some("/"));
    }

    #[test]
    fn same_site_parse_rejects_garbage() {
        assert!(SameSite::parse("relaxed").is_none());
        assert!(SameSite::parse("").is_none());
    }

    #[test]
    fn same_site_as_str_canonical_spelling() {
        assert_eq!(SameSite::Strict.as_str(), "Strict");
        assert_eq!(SameSite::Lax.as_str(), "Lax");
        assert_eq!(SameSite::None.as_str(), "None");
    }

    #[test]
    fn negative_max_age_round_trips() {
        let header = Cookie::builder("k", "v")
            .max_age(-1)
            .build()
            .to_header_value();
        assert!(header.contains("Max-Age=-1"));
        let parsed = parse_set_cookie(&header).expect("parse");
        assert_eq!(parsed.max_age, Some(-1));
    }
}
