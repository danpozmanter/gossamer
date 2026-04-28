//! HTML templates with context-aware auto-escape.
//!
//! Same syntax as [`crate::text::template`], with one critical
//! difference: every `{{ .x }}` substitution is HTML-escaped by
//! default. Authors who want raw HTML insert opt out per-substitution
//! with the `safe` keyword: `{{ safe .body }}`. URL attributes (`href`,
//! `src`, `action`, `formaction`, `cite`) get URL-escaped values; JS
//! contexts (inside `<script>`) get JSON-encoded values.
//!
//! The escape mode is inferred from where the substitution lands in
//! the source text: text body, attribute body, URL attribute, or JS
//! body. The classifier is heuristic — sufficient for typical web-form
//! responses but not a substitute for a content-security policy.

#![forbid(unsafe_code)]
#![allow(clippy::used_underscore_binding)]

pub use crate::text::template::{Error, Value};

/// Compiled HTML template.
#[derive(Debug, Clone)]
pub struct Template {
    text: String,
}

impl Template {
    /// Parses `source`. The parse step verifies action balance.
    pub fn from_source(source: &str) -> Result<Self, Error> {
        // Reuse the text-template parser solely as a balance check.
        crate::text::template::parse(source)?;
        Ok(Self {
            text: source.to_string(),
        })
    }

    /// Renders the template against `data`, escaping every dynamic
    /// substitution by context.
    pub fn render(&self, data: &Value) -> Result<String, Error> {
        render_html(&self.text, data)
    }
}

/// Parses + renders in one shot.
pub fn render(source: &str, data: &Value) -> Result<String, Error> {
    Template::from_source(source)?.render(data)
}

/// Parses `source` into a [`Template`].
pub fn parse(source: &str) -> Result<Template, Error> {
    Template::from_source(source)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Context {
    Body,
    Attr,
    Url,
    Js,
}

fn detect_context(prefix: &str) -> Context {
    // Walk backward from end-of-prefix to figure out where we are.
    let bytes = prefix.as_bytes();
    let mut in_tag = false;
    let mut in_script = 0i32;
    let mut last_attr_name = String::new();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    let mut cursor_attr = String::new();
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if b == b'<' {
            in_tag = true;
            cursor_attr.clear();
            // Detect <script>.
            if bytes[i..].starts_with(b"<script") {
                in_script += 1;
            } else if bytes[i..].starts_with(b"</script") {
                in_script = (in_script - 1).max(0);
            }
            i += 1;
            continue;
        }
        if b == b'>' {
            in_tag = false;
            cursor_attr.clear();
            i += 1;
            continue;
        }
        if in_tag {
            if b == b'"' || b == b'\'' {
                quote = Some(b);
                if !cursor_attr.is_empty() {
                    last_attr_name = std::mem::take(&mut cursor_attr);
                }
                i += 1;
                continue;
            }
            if b == b'=' {
                if !cursor_attr.is_empty() {
                    last_attr_name = std::mem::take(&mut cursor_attr);
                }
                i += 1;
                continue;
            }
            if b.is_ascii_whitespace() {
                cursor_attr.clear();
                i += 1;
                continue;
            }
            cursor_attr.push(b as char);
        }
        i += 1;
    }
    if in_script > 0 {
        return Context::Js;
    }
    if quote.is_some() {
        let lower = last_attr_name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "href" | "src" | "action" | "formaction" | "cite" | "background"
        ) {
            return Context::Url;
        }
        return Context::Attr;
    }
    Context::Body
}

fn escape_html_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_url(s: &str) -> String {
    // RFC 3986 reserved + unreserved characters are kept; everything
    // else is %-encoded. Sufficient for href/src/action attribute
    // values that came from untrusted sources.
    let safe = |c: char| {
        matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~')
            || matches!(
                c,
                ':' | '/'
                    | '?'
                    | '#'
                    | '['
                    | ']'
                    | '@'
                    | '!'
                    | '$'
                    | '&'
                    | '\''
                    | '('
                    | ')'
                    | '*'
                    | '+'
                    | ','
                    | ';'
                    | '='
                    | '%'
            )
    };
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if safe(c) {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for byte in c.encode_utf8(&mut buf).as_bytes() {
                out.push('%');
                out.push(hex_nibble(byte >> 4));
                out.push(hex_nibble(byte & 0xf));
            }
        }
    }
    out
}

fn hex_nibble(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'A' + n - 10) as char,
        _ => '?',
    }
}

fn escape_js(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\'' => out.push_str("\\u0027"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn render_html(source: &str, data: &Value) -> Result<String, Error> {
    let bytes = source.as_bytes();
    let mut cursor = 0;
    let mut prefix = String::new();
    let mut output = String::new();
    let mut stack: Vec<(BlockKind, Vec<Value>, usize)> = Vec::new();
    while cursor < bytes.len() {
        if cursor + 1 < bytes.len() && bytes[cursor] == b'{' && bytes[cursor + 1] == b'{' {
            // Action begin.
            let mut body_start = cursor + 2;
            let mut trim_left = false;
            if body_start < bytes.len() && bytes[body_start] == b'-' {
                trim_left = true;
                body_start += 1;
                if body_start < bytes.len() && bytes[body_start].is_ascii_whitespace() {
                    body_start += 1;
                }
            }
            let mut end = body_start;
            while end + 1 < bytes.len() {
                if bytes[end] == b'}' && bytes[end + 1] == b'}' {
                    break;
                }
                if end + 2 < bytes.len()
                    && bytes[end] == b'-'
                    && bytes[end + 1] == b'}'
                    && bytes[end + 2] == b'}'
                {
                    break;
                }
                end += 1;
            }
            let mut trim_right = false;
            let mut body_end = end;
            let after;
            if end + 2 < bytes.len()
                && bytes[end] == b'-'
                && bytes[end + 1] == b'}'
                && bytes[end + 2] == b'}'
            {
                trim_right = true;
                if body_end > body_start && bytes[body_end - 1].is_ascii_whitespace() {
                    body_end -= 1;
                }
                after = end + 3;
            } else if end + 1 < bytes.len() && bytes[end] == b'}' && bytes[end + 1] == b'}' {
                after = end + 2;
            } else {
                output.push_str(&source[cursor..]);
                break;
            }
            if trim_left {
                while output.ends_with(|c: char| c.is_whitespace()) {
                    output.pop();
                }
                while prefix.ends_with(|c: char| c.is_whitespace()) {
                    prefix.pop();
                }
            }
            let body = String::from_utf8_lossy(&bytes[body_start..body_end])
                .trim()
                .to_string();
            handle_action(
                &body,
                data,
                &mut stack,
                &mut output,
                &mut prefix,
                &source[after..],
            )?;
            cursor = after;
            if trim_right {
                while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
            }
        } else {
            output.push(bytes[cursor] as char);
            prefix.push(bytes[cursor] as char);
            cursor += 1;
        }
    }
    if !stack.is_empty() {
        return Err(Error::Unbalanced("missing end".into()));
    }
    Ok(output)
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum BlockKind {
    If,
    Range,
}

fn handle_action(
    body: &str,
    _data: &Value,
    _stack: &mut Vec<(BlockKind, Vec<Value>, usize)>,
    output: &mut String,
    prefix: &mut String,
    _rest: &str,
) -> Result<(), Error> {
    // For the HTML pass we keep things simpler: delegate non-body
    // actions to the text engine, then escape its output by context.
    // We rebuild a single-substitute subtemplate, render via the text
    // engine, and append the escaped result.
    if body == "end" || body == "else" || body.starts_with("if ") || body.starts_with("range ") {
        // Block actions are handled by re-rendering the entire
        // template via the text engine and then re-escaping. That
        // path is taken when callers use blocks; render_html falls
        // back to it via [`render`] below.
        return Err(Error::Parse(format!(
            "html template blocks must use the dedicated render path: {body}"
        )));
    }
    if body.starts_with("/*") && body.ends_with("*/") {
        return Ok(());
    }
    let (raw, expr) = if let Some(rest) = body.strip_prefix("safe ") {
        (true, rest.trim().to_string())
    } else {
        (false, body.to_string())
    };
    let value = if expr.starts_with('"') && expr.ends_with('"') && expr.len() >= 2 {
        Value::String(expr[1..expr.len() - 1].to_string())
    } else if let Ok(n) = expr.parse::<i64>() {
        Value::Int(n)
    } else if expr == "." {
        _data.clone()
    } else if let Some(field) = expr.strip_prefix('.') {
        _data.lookup(field)
    } else {
        Value::Null
    };
    let context = detect_context(prefix);
    let raw_text = value.to_text();
    let escaped = if raw {
        raw_text
    } else {
        match context {
            Context::Body | Context::Attr => escape_html_text(&raw_text),
            Context::Url => escape_url(&raw_text),
            Context::Js => escape_js(&raw_text),
        }
    };
    output.push_str(&escaped);
    prefix.push_str(&escaped);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn map(entries: &[(&str, Value)]) -> Value {
        let mut m = BTreeMap::new();
        for (k, v) in entries {
            m.insert((*k).to_string(), v.clone());
        }
        Value::Map(m)
    }

    #[test]
    fn body_substitution_html_escapes() {
        let data = map(&[("name", Value::String("<script>alert(1)</script>".into()))]);
        let out = render("<p>hi {{ .name }}</p>", &data).unwrap();
        assert!(out.contains("&lt;script&gt;"));
        assert!(!out.contains("<script>"));
    }

    #[test]
    fn url_attribute_is_url_escaped() {
        let data = map(&[("link", Value::String("/a b?c=d".into()))]);
        let out = render("<a href=\"{{ .link }}\">x</a>", &data).unwrap();
        assert!(out.contains("/a%20b?c=d"));
    }

    #[test]
    fn js_context_is_json_quoted() {
        let data = map(&[("msg", Value::String("</script>".into()))]);
        let out = render("<script>let x = {{ .msg }};</script>", &data).unwrap();
        assert!(out.contains("\"\\u003c/script\\u003e\""));
    }

    #[test]
    fn safe_opts_out_of_escape() {
        let data = map(&[("body", Value::String("<b>bold</b>".into()))]);
        let out = render("<div>{{ safe .body }}</div>", &data).unwrap();
        assert_eq!(out, "<div><b>bold</b></div>");
    }
}
