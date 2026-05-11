//! HTML-template support and string escaping utilities.

#![forbid(unsafe_code)]

#[cfg(feature = "templates")]
pub mod template;

/// Escapes `s` for safe insertion into HTML text or attribute values.
///
/// Replaces `&`, `<`, `>`, `"`, and `'` with their named entity equivalents.
#[must_use]
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Unescapes named and numeric HTML entities back to their character equivalents.
///
/// Handles the five standard named entities (`&amp;`, `&lt;`, `&gt;`,
/// `&quot;`, `&apos;`) plus decimal (`&#NNN;`) and hex (`&#xHHH;`) references.
#[must_use]
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '&' {
            out.push(ch);
            continue;
        }
        // Collect the entity up to ';'.
        let mut entity = String::new();
        let mut closed = false;
        for _ in 0..16 {
            match chars.next() {
                Some(';') => {
                    closed = true;
                    break;
                }
                Some(c) => entity.push(c),
                None => break,
            }
        }
        if !closed {
            out.push('&');
            out.push_str(&entity);
            continue;
        }
        let decoded = match entity.as_str() {
            "amp" => "&",
            "lt" => "<",
            "gt" => ">",
            "quot" => "\"",
            "apos" | "#39" => "'",
            "nbsp" => "\u{00A0}",
            s if s.starts_with("#x") || s.starts_with("#X") => {
                let n = u32::from_str_radix(&s[2..], 16)
                    .ok()
                    .and_then(char::from_u32);
                if let Some(c) = n {
                    out.push(c);
                } else {
                    out.push('&');
                    out.push_str(&entity);
                    out.push(';');
                }
                continue;
            }
            s if s.starts_with('#') => {
                let n = s[1..].parse::<u32>().ok().and_then(char::from_u32);
                if let Some(c) = n {
                    out.push(c);
                } else {
                    out.push('&');
                    out.push_str(&entity);
                    out.push(';');
                }
                continue;
            }
            _ => {
                out.push('&');
                out.push_str(&entity);
                out.push(';');
                continue;
            }
        };
        out.push_str(decoded);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_special_chars() {
        assert_eq!(
            escape("<b>Hello & 'World'</b>"),
            "&lt;b&gt;Hello &amp; &#39;World&#39;&lt;/b&gt;"
        );
    }

    #[test]
    fn escape_quotes() {
        assert_eq!(escape("say \"hi\""), "say &quot;hi&quot;");
    }

    #[test]
    fn unescape_named_entities() {
        assert_eq!(
            unescape("&lt;b&gt;Hello &amp; World&lt;/b&gt;"),
            "<b>Hello & World</b>"
        );
    }

    #[test]
    fn unescape_numeric_decimal() {
        assert_eq!(unescape("&#65;"), "A");
    }

    #[test]
    fn unescape_numeric_hex() {
        assert_eq!(unescape("&#x41;"), "A");
    }

    #[test]
    fn escape_unescape_roundtrip() {
        let original = "<script>alert('XSS & \"danger\"');</script>";
        assert_eq!(unescape(&escape(original)), original);
    }
}
