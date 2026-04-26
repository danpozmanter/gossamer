//! Runtime support for `std::net::url`.
//! Minimal URL parser covering scheme, host, port, path, query, and
//! fragment. Deliberately narrower than `url` crate: enough for HTTP
//! client code and the package manager, without pulling in IDNA or
//! Unicode normalisation.

#![forbid(unsafe_code)]

use crate::errors::Error;

/// Parsed URL.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Url {
    /// Scheme (`http`, `https`, `file`, ...), lowercase.
    pub scheme: String,
    /// Host component (without port).
    pub host: String,
    /// Optional port.
    pub port: Option<u16>,
    /// Path, always starting with `/` when present.
    pub path: String,
    /// Raw query string, excluding the `?` sentinel.
    pub query: String,
    /// Fragment, excluding the `#` sentinel.
    pub fragment: String,
}

impl Url {
    /// Parses a string into a [`Url`]. Accepts `scheme://host[:port]
    /// /path?query#fragment`.
    pub fn parse(input: &str) -> Result<Self, Error> {
        let mut rest = input;
        let scheme = match rest.find("://") {
            Some(idx) => {
                let scheme = rest[..idx].to_ascii_lowercase();
                rest = &rest[idx + 3..];
                scheme
            }
            None => return Err(Error::new(format!("missing scheme in `{input}`"))),
        };
        let (authority, tail) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, ""),
        };
        let (host, port) = split_host_port(authority)?;
        let mut path = String::new();
        let mut query = String::new();
        let mut fragment = String::new();
        let mut cursor = tail;
        if let Some(idx) = cursor.find('#') {
            fragment = cursor[idx + 1..].to_string();
            cursor = &cursor[..idx];
        }
        if let Some(idx) = cursor.find('?') {
            query = cursor[idx + 1..].to_string();
            cursor = &cursor[..idx];
        }
        path.push_str(cursor);
        Ok(Self {
            scheme,
            host,
            port,
            path,
            query,
            fragment,
        })
    }

    /// Renders the URL back to its canonical string form.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!("{}://{}", self.scheme, self.host);
        if let Some(port) = self.port {
            out.push(':');
            out.push_str(&port.to_string());
        }
        out.push_str(&self.path);
        if !self.query.is_empty() {
            out.push('?');
            out.push_str(&self.query);
        }
        if !self.fragment.is_empty() {
            out.push('#');
            out.push_str(&self.fragment);
        }
        out
    }
}

fn split_host_port(authority: &str) -> Result<(String, Option<u16>), Error> {
    match authority.rfind(':') {
        Some(idx) if !authority[idx + 1..].contains(']') => {
            let host = authority[..idx].to_string();
            let port: u16 = authority[idx + 1..]
                .parse()
                .map_err(|_| Error::new(format!("invalid port in `{authority}`")))?;
            Ok((host, Some(port)))
        }
        _ => Ok((authority.to_string(), None)),
    }
}

/// Escapes `text` for use in a URL query parameter.
#[must_use]
pub fn query_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        let b = *byte;
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else if b == b' ' {
            out.push('+');
        } else {
            out.push('%');
            out.push(upper_hex(b >> 4));
            out.push(upper_hex(b & 0xf));
        }
    }
    out
}

/// Inverts [`query_escape`].
pub fn query_unescape(text: &str) -> Result<String, Error> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                if i + 2 >= bytes.len() {
                    return Err(Error::new("truncated percent-escape"));
                }
                let hi = hex_value(bytes[i + 1]).ok_or_else(|| Error::new("bad hex"))?;
                let lo = hex_value(bytes[i + 2]).ok_or_else(|| Error::new("bad hex"))?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| Error::new("non-UTF-8 percent-escape"))
}

/// Encodes `pairs` as `key=value&key=value` query string.
#[must_use]
pub fn encode_query(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(&query_escape(k));
        out.push('=');
        out.push_str(&query_escape(v));
    }
    out
}

/// Decodes a query string into a `(key, value)` list, preserving
/// source order.
pub fn decode_query(raw: &str) -> Result<Vec<(String, String)>, Error> {
    let mut out = Vec::new();
    if raw.is_empty() {
        return Ok(out);
    }
    for pair in raw.split('&') {
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        out.push((query_unescape(k)?, query_unescape(v)?));
    }
    Ok(out)
}

const fn upper_hex(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'A' + n - 10) as char,
        _ => '?',
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_url() {
        let u = Url::parse("https://example.com:443/a/b?k=v#frag").unwrap();
        assert_eq!(u.scheme, "https");
        assert_eq!(u.host, "example.com");
        assert_eq!(u.port, Some(443));
        assert_eq!(u.path, "/a/b");
        assert_eq!(u.query, "k=v");
        assert_eq!(u.fragment, "frag");
    }

    #[test]
    fn parse_without_port_and_path() {
        let u = Url::parse("http://example.com").unwrap();
        assert_eq!(u.host, "example.com");
        assert!(u.port.is_none());
        assert_eq!(u.path, "");
    }

    #[test]
    fn parse_rejects_missing_scheme() {
        assert!(Url::parse("example.com").is_err());
    }

    #[test]
    fn render_round_trips_url() {
        let input = "http://example.com:8080/path?a=1&b=2#x";
        let u = Url::parse(input).unwrap();
        assert_eq!(u.render(), input);
    }

    #[test]
    fn query_escape_and_unescape_round_trip() {
        let raw = "hello world/!*'";
        let escaped = query_escape(raw);
        assert_eq!(escaped, "hello+world%2F%21%2A%27");
        assert_eq!(query_unescape(&escaped).unwrap(), raw);
    }

    #[test]
    fn encode_and_decode_query_pairs() {
        let encoded = encode_query(&[("name", "jane doe"), ("age", "30")]);
        assert_eq!(encoded, "name=jane+doe&age=30");
        let decoded = decode_query(&encoded).unwrap();
        assert_eq!(
            decoded,
            vec![
                ("name".to_string(), "jane doe".to_string()),
                ("age".to_string(), "30".to_string()),
            ]
        );
    }
}
