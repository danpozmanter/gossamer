//! Plain-text templates.
//!
//! Source-compatible enough with Go's `text/template` for most simple
//! interpolation and conditional flows, without dragging in a full
//! action language. Supported syntax:
//!
//! - `{{ .field }}` — looks up `field` in the data map.
//! - `{{ .a.b }}` — chained map lookups.
//! - `{{ if .x }}…{{ else }}…{{ end }}` — boolean branch.
//! - `{{ range .items }}…{{ end }}` — iterate a sequence; inside the
//!   loop, `.` refers to the current item and parent fields are still
//!   reachable via stack-search.
//! - `{{ "literal" }}` — string literal.
//! - `{{- .x -}}` / `{{- if -}}` — strip surrounding whitespace.
//! - Comments: `{{/* anything */}}`.
//!
//! No custom function pipelines, no auto-escape (use
//! [`crate::html::template`] when emitting HTML).

#![forbid(unsafe_code)]
// The render loop walks one byte at a time over the template body,
// branching per `{{ }}` action shape; the flat match-on-action keeps
// the parser's intent in a single readable scan.
#![allow(clippy::too_many_lines)]

use std::collections::BTreeMap;

use thiserror::Error;

/// Dynamic value passed into a template. Mirrors the JSON Value
/// shape so callers can plumb either source through unchanged.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `null` / missing-key sentinel.
    Null,
    /// Boolean.
    Bool(bool),
    /// 64-bit signed integer.
    Int(i64),
    /// 64-bit floating-point.
    Float(f64),
    /// UTF-8 string.
    String(String),
    /// Ordered sequence.
    Seq(Vec<Value>),
    /// Ordered map keyed by name.
    Map(BTreeMap<String, Value>),
}

impl Value {
    /// Owned string form (used by `{{ .x }}` rendering).
    #[must_use]
    pub fn to_text(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Bool(b) => b.to_string(),
            Value::Int(n) => n.to_string(),
            Value::Float(n) => format!("{n}"),
            Value::String(s) => s.clone(),
            Value::Seq(items) => {
                let mut out = String::new();
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    out.push_str(&v.to_text());
                }
                out
            }
            Value::Map(_) => format!("{self:?}"),
        }
    }

    /// `if` truthiness, mirroring Go's template semantics.
    #[must_use]
    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Float(n) => *n != 0.0,
            Value::String(s) => !s.is_empty(),
            Value::Seq(items) => !items.is_empty(),
            Value::Map(m) => !m.is_empty(),
        }
    }

    /// Chases a `.a.b.c` lookup. Returns [`Value::Null`] for any
    /// missing intermediate.
    #[must_use]
    pub fn lookup(&self, path: &str) -> Value {
        let mut cursor = self.clone();
        for segment in path.split('.').filter(|s| !s.is_empty()) {
            cursor = match cursor {
                Value::Map(m) => m.get(segment).cloned().unwrap_or(Value::Null),
                _ => Value::Null,
            };
        }
        cursor
    }
}

/// Parsed template AST.
#[derive(Debug, Clone)]
pub struct Template {
    nodes: Vec<Node>,
}

#[derive(Debug, Clone)]
enum Node {
    Text(String),
    Substitute(String),
    If {
        cond: String,
        then_body: Vec<Node>,
        else_body: Vec<Node>,
    },
    Range {
        path: String,
        body: Vec<Node>,
    },
}

/// Errors raised by the parser / renderer.
#[derive(Debug, Clone, Error)]
pub enum Error {
    /// Source did not parse.
    #[error("template: {0}")]
    Parse(String),
    /// `{{ end }}` arrived without a matching opener (or vice versa).
    #[error("template: unbalanced action: {0}")]
    Unbalanced(String),
}

/// Parses a template source.
pub fn parse(source: &str) -> Result<Template, Error> {
    let tokens = tokenize(source);
    let mut iter = tokens.into_iter().peekable();
    let nodes = parse_block(&mut iter, false)?;
    if iter.peek().is_some() {
        return Err(Error::Unbalanced("trailing tokens".into()));
    }
    Ok(Template { nodes })
}

impl Template {
    /// Parses `source` directly, returning a [`Template`].
    pub fn from_source(source: &str) -> Result<Self, Error> {
        parse(source)
    }

    /// Renders the template against `data`.
    pub fn render(&self, data: &Value) -> Result<String, Error> {
        let mut out = String::with_capacity(64);
        render_block(&self.nodes, std::slice::from_ref(data), &mut out)?;
        Ok(out)
    }
}

/// Convenience renderer.
pub fn render(source: &str, data: &Value) -> Result<String, Error> {
    let tpl = parse(source)?;
    tpl.render(data)
}

/// Writes the rendered template into `out`. Useful for streaming.
pub fn render_to(template: &Template, data: &Value, out: &mut String) -> Result<(), Error> {
    render_block(&template.nodes, std::slice::from_ref(data), out)
}

#[derive(Debug, Clone)]
enum Token {
    Text(String),
    Action {
        body: String,
        trim_left: bool,
        trim_right: bool,
    },
}

fn tokenize(source: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let bytes = source.as_bytes();
    let mut cursor = 0;
    let mut text_start = 0;
    while cursor < bytes.len() {
        if cursor + 1 < bytes.len() && bytes[cursor] == b'{' && bytes[cursor + 1] == b'{' {
            if cursor > text_start {
                tokens.push(Token::Text(
                    String::from_utf8_lossy(&bytes[text_start..cursor]).into_owned(),
                ));
            }
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
            if end + 2 < bytes.len()
                && bytes[end] == b'-'
                && bytes[end + 1] == b'}'
                && bytes[end + 2] == b'}'
            {
                trim_right = true;
                if body_end > body_start && bytes[body_end - 1].is_ascii_whitespace() {
                    body_end -= 1;
                }
                cursor = end + 3;
            } else if end + 1 < bytes.len() && bytes[end] == b'}' && bytes[end + 1] == b'}' {
                cursor = end + 2;
            } else {
                tokens.push(Token::Text(
                    String::from_utf8_lossy(&bytes[text_start..]).into_owned(),
                ));
                return tokens;
            }
            let body = String::from_utf8_lossy(&bytes[body_start..body_end])
                .trim()
                .to_string();
            tokens.push(Token::Action {
                body,
                trim_left,
                trim_right,
            });
            text_start = cursor;
        } else {
            cursor += 1;
        }
    }
    if text_start < bytes.len() {
        tokens.push(Token::Text(
            String::from_utf8_lossy(&bytes[text_start..]).into_owned(),
        ));
    }
    apply_trims(&mut tokens);
    tokens
}

fn apply_trims(tokens: &mut [Token]) {
    let mut i = 0;
    while i < tokens.len() {
        let (trim_left, trim_right) = if let Token::Action {
            trim_left,
            trim_right,
            ..
        } = &tokens[i]
        {
            (*trim_left, *trim_right)
        } else {
            (false, false)
        };
        if trim_left && i > 0 {
            if let Token::Text(t) = &mut tokens[i - 1] {
                while t.ends_with(|c: char| c.is_whitespace()) {
                    t.pop();
                }
            }
        }
        if trim_right && i + 1 < tokens.len() {
            if let Token::Text(t) = &mut tokens[i + 1] {
                let trimmed = t
                    .trim_start_matches(|c: char| c.is_whitespace())
                    .to_string();
                *t = trimmed;
            }
        }
        i += 1;
    }
}

fn parse_block<I>(
    tokens: &mut std::iter::Peekable<I>,
    inside_block: bool,
) -> Result<Vec<Node>, Error>
where
    I: Iterator<Item = Token>,
{
    let mut nodes = Vec::new();
    while let Some(tok) = tokens.peek() {
        match tok {
            Token::Text(_) => {
                if let Some(Token::Text(t)) = tokens.next() {
                    nodes.push(Node::Text(t));
                }
            }
            Token::Action { body, .. } => {
                let body = body.clone();
                let trimmed = body.trim();
                if trimmed == "end" || trimmed == "else" {
                    if !inside_block {
                        return Err(Error::Unbalanced(trimmed.to_string()));
                    }
                    return Ok(nodes);
                }
                tokens.next();
                if trimmed.starts_with("/*") && trimmed.ends_with("*/") {
                    continue;
                }
                if let Some(rest) = trimmed.strip_prefix("if ") {
                    let cond = rest.trim().to_string();
                    let then_body = parse_block(tokens, true)?;
                    let mut else_body = Vec::new();
                    if let Some(Token::Action { body, .. }) = tokens.peek() {
                        if body.trim() == "else" {
                            tokens.next();
                            else_body = parse_block(tokens, true)?;
                        }
                    }
                    let closer = tokens.next();
                    match closer {
                        Some(Token::Action { body, .. }) if body.trim() == "end" => {}
                        _ => return Err(Error::Unbalanced("expected end after if".into())),
                    }
                    nodes.push(Node::If {
                        cond,
                        then_body,
                        else_body,
                    });
                } else if let Some(rest) = trimmed.strip_prefix("range ") {
                    let path = rest.trim().to_string();
                    let body = parse_block(tokens, true)?;
                    let closer = tokens.next();
                    match closer {
                        Some(Token::Action { body, .. }) if body.trim() == "end" => {}
                        _ => return Err(Error::Unbalanced("expected end after range".into())),
                    }
                    nodes.push(Node::Range { path, body });
                } else {
                    nodes.push(Node::Substitute(trimmed.to_string()));
                }
            }
        }
    }
    Ok(nodes)
}

fn resolve(stack: &[Value], expr: &str) -> Value {
    let expr = expr.trim();
    if expr.starts_with('"') && expr.ends_with('"') && expr.len() >= 2 {
        return Value::String(expr[1..expr.len() - 1].to_string());
    }
    if let Ok(n) = expr.parse::<i64>() {
        return Value::Int(n);
    }
    if expr == "." {
        return stack.last().cloned().unwrap_or(Value::Null);
    }
    if let Some(rest) = expr.strip_prefix('.') {
        if let Some(top) = stack.last() {
            let lookup = top.lookup(rest);
            if !matches!(lookup, Value::Null) {
                return lookup;
            }
        }
        for frame in stack.iter().rev().skip(1) {
            let lookup = frame.lookup(rest);
            if !matches!(lookup, Value::Null) {
                return lookup;
            }
        }
        return Value::Null;
    }
    Value::Null
}

fn render_block(nodes: &[Node], stack: &[Value], out: &mut String) -> Result<(), Error> {
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Substitute(expr) => {
                let value = resolve(stack, expr);
                out.push_str(&value.to_text());
            }
            Node::If {
                cond,
                then_body,
                else_body,
            } => {
                let value = resolve(stack, cond);
                if value.truthy() {
                    render_block(then_body, stack, out)?;
                } else {
                    render_block(else_body, stack, out)?;
                }
            }
            Node::Range { path, body } => {
                let value = resolve(stack, path);
                if let Value::Seq(items) = value {
                    for item in items {
                        let mut child_stack: Vec<Value> = stack.to_vec();
                        child_stack.push(item);
                        render_block(body, &child_stack, out)?;
                    }
                } else if let Value::Map(map) = value {
                    for (k, v) in map {
                        let mut framed = BTreeMap::new();
                        framed.insert("Key".to_string(), Value::String(k));
                        framed.insert("Value".to_string(), v);
                        let mut child_stack: Vec<Value> = stack.to_vec();
                        child_stack.push(Value::Map(framed));
                        render_block(body, &child_stack, out)?;
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(entries: &[(&str, Value)]) -> Value {
        let mut m = BTreeMap::new();
        for (k, v) in entries {
            m.insert((*k).to_string(), v.clone());
        }
        Value::Map(m)
    }

    #[test]
    fn renders_plain_text() {
        let out = render("hello world", &Value::Null).unwrap();
        assert_eq!(out, "hello world");
    }

    #[test]
    fn substitutes_field() {
        let data = map(&[("name", Value::String("gossamer".into()))]);
        let out = render("hello {{ .name }}", &data).unwrap();
        assert_eq!(out, "hello gossamer");
    }

    #[test]
    fn handles_if_else() {
        let yes = map(&[("ok", Value::Bool(true))]);
        let no = map(&[("ok", Value::Bool(false))]);
        let tpl = "{{ if .ok }}YES{{ else }}NO{{ end }}";
        assert_eq!(render(tpl, &yes).unwrap(), "YES");
        assert_eq!(render(tpl, &no).unwrap(), "NO");
    }

    #[test]
    fn handles_range_over_seq() {
        let data = map(&[(
            "items",
            Value::Seq(vec![
                Value::String("a".into()),
                Value::String("b".into()),
                Value::String("c".into()),
            ]),
        )]);
        let tpl = "{{ range .items }}[{{ . }}]{{ end }}";
        assert_eq!(render(tpl, &data).unwrap(), "[a][b][c]");
    }

    #[test]
    fn whitespace_trim_works() {
        let data = map(&[("x", Value::Int(42))]);
        let tpl = "before  {{- .x -}}  after";
        assert_eq!(render(tpl, &data).unwrap(), "before42after");
    }

    #[test]
    fn comments_are_dropped() {
        let out = render("a{{/* comment */}}b", &Value::Null).unwrap();
        assert_eq!(out, "ab");
    }

    #[test]
    fn nested_lookup() {
        let inner = map(&[("name", Value::String("nested".into()))]);
        let outer = map(&[("user", inner)]);
        let out = render("hi {{ .user.name }}", &outer).unwrap();
        assert_eq!(out, "hi nested");
    }
}
