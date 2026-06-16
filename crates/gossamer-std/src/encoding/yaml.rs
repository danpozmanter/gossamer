//! YAML 1.2 parser and emitter, exposed at `std::encoding::yaml`.
//!
//! Backed by `serde_yaml` for the heavy lifting; the wrapper preserves
//! the same dynamic-`Value` shape the rest of the stdlib uses for
//! JSON, so callers can `match` on tag and traverse maps and arrays
//! the same way regardless of source format. Multi-document streams
//! are supported via [`parse_all`] / [`encode_all`].

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use thiserror::Error;

const DEFAULT_MAX_DEPTH: usize = 128;
const DEFAULT_MAX_SIZE: usize = 16 * 1024 * 1024;

static MAX_DEPTH: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_DEPTH);
static MAX_SIZE: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_SIZE);

/// Overrides the process-wide cap on parser nesting depth.
pub fn set_max_depth(n: usize) {
    MAX_DEPTH.store(n, Ordering::Relaxed);
}

/// Overrides the process-wide cap on parser input bytes.
pub fn set_max_size(n: usize) {
    MAX_SIZE.store(n, Ordering::Relaxed);
}

/// Current cap on parser nesting depth.
#[must_use]
pub fn max_depth() -> usize {
    MAX_DEPTH.load(Ordering::Relaxed)
}

/// Current cap on parser input bytes.
#[must_use]
pub fn max_size() -> usize {
    MAX_SIZE.load(Ordering::Relaxed)
}

fn check_depth(value: &serde_yaml::Value, depth: usize, cap: usize) -> Result<(), Error> {
    if depth > cap {
        return Err(Error {
            message: format!("nesting depth exceeds max_depth ({cap})"),
        });
    }
    match value {
        serde_yaml::Value::Sequence(seq) => {
            for v in seq {
                check_depth(v, depth + 1, cap)?;
            }
        }
        serde_yaml::Value::Mapping(map) => {
            for (_, v) in map {
                check_depth(v, depth + 1, cap)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Dynamically typed YAML value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `null` / `~` / missing-value scalar.
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// 64-bit signed integer scalar.
    Int(i64),
    /// 64-bit floating-point scalar.
    Float(f64),
    /// UTF-8 string scalar.
    String(String),
    /// Ordered sequence (`[a, b, c]`).
    Seq(Vec<Value>),
    /// Ordered mapping (insertion order preserved).
    Map(Vec<(Value, Value)>),
}

impl Value {
    /// Returns the string when `self` is a string scalar.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        if let Value::String(s) = self {
            Some(s)
        } else {
            None
        }
    }

    /// Returns the int when `self` is an int scalar.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        if let Value::Int(n) = self {
            Some(*n)
        } else {
            None
        }
    }

    /// Returns the float when `self` is a float scalar (or int -
    /// numeric promotion).
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(n) => Some(*n),
            Value::Int(n) => Some(*n as f64),
            _ => None,
        }
    }

    /// Returns the bool when `self` is a bool scalar.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        if let Value::Bool(b) = self {
            Some(*b)
        } else {
            None
        }
    }

    /// Returns the inner sequence when `self` is a sequence.
    #[must_use]
    pub fn as_seq(&self) -> Option<&[Value]> {
        if let Value::Seq(items) = self {
            Some(items)
        } else {
            None
        }
    }

    /// Returns the inner map when `self` is a map.
    #[must_use]
    pub fn as_map(&self) -> Option<&[(Value, Value)]> {
        if let Value::Map(items) = self {
            Some(items)
        } else {
            None
        }
    }

    /// Looks up `key` in a map. Returns `None` for non-maps or when
    /// the key isn't present. Compares string keys directly so the
    /// common `value.get("name")` shape works.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        let entries = self.as_map()?;
        entries.iter().find_map(|(k, v)| {
            if k.as_str() == Some(key) {
                Some(v)
            } else {
                None
            }
        })
    }
}

/// Error returned by the YAML parser.
#[derive(Debug, Clone, Error)]
#[error("yaml: {message}")]
pub struct Error {
    /// Human-readable explanation.
    pub message: String,
}

impl Error {
    fn from_serde(err: serde_yaml::Error) -> Self {
        Self {
            message: err.to_string(),
        }
    }
}

/// Parses a single YAML document into a [`Value`].
pub fn parse(source: &str) -> Result<Value, Error> {
    let size_cap = max_size();
    if source.len() > size_cap {
        return Err(Error {
            message: format!("input exceeds max_size ({} > {size_cap})", source.len()),
        });
    }
    let raw: serde_yaml::Value = serde_yaml::from_str(source).map_err(Error::from_serde)?;
    check_depth(&raw, 0, max_depth())?;
    Ok(from_serde(raw))
}

/// Parses every document in a multi-document YAML stream.
pub fn parse_all(source: &str) -> Result<Vec<Value>, Error> {
    let size_cap = max_size();
    if source.len() > size_cap {
        return Err(Error {
            message: format!("input exceeds max_size ({} > {size_cap})", source.len()),
        });
    }
    let depth_cap = max_depth();
    let mut out = Vec::new();
    for doc in serde_yaml::Deserializer::from_str(source) {
        let value = serde_yaml::Value::deserialize(doc).map_err(Error::from_serde)?;
        check_depth(&value, 0, depth_cap)?;
        out.push(from_serde(value));
    }
    Ok(out)
}

/// Encodes a [`Value`] as a YAML document (no leading `---`).
pub fn encode(value: &Value) -> Result<String, Error> {
    let serde_value = to_serde(value);
    serde_yaml::to_string(&serde_value).map_err(Error::from_serde)
}

/// Encodes a slice of values as a multi-document YAML stream.
pub fn encode_all(values: &[Value]) -> Result<String, Error> {
    let mut out = String::new();
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.push_str("---\n");
        }
        out.push_str(&encode(v)?);
    }
    Ok(out)
}

fn from_serde(value: serde_yaml::Value) -> Value {
    match value {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(b) => Value::Bool(b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(u) = n.as_u64() {
                if let Ok(signed) = i64::try_from(u) {
                    Value::Int(signed)
                } else {
                    Value::Float(u as f64)
                }
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        serde_yaml::Value::String(s) => Value::String(s),
        serde_yaml::Value::Sequence(items) => {
            Value::Seq(items.into_iter().map(from_serde).collect())
        }
        serde_yaml::Value::Mapping(map) => {
            let entries = map
                .into_iter()
                .map(|(k, v)| (from_serde(k), from_serde(v)))
                .collect();
            Value::Map(entries)
        }
        serde_yaml::Value::Tagged(boxed) => {
            // Drop tag, preserve the inner value. Track B doesn't
            // expose explicit tag handling yet - most user code uses
            // YAML as a JSON-shaped data format.
            from_serde(boxed.value)
        }
    }
}

fn to_serde(value: &Value) -> serde_yaml::Value {
    match value {
        Value::Null => serde_yaml::Value::Null,
        Value::Bool(b) => serde_yaml::Value::Bool(*b),
        Value::Int(n) => serde_yaml::Value::Number((*n).into()),
        Value::Float(n) => serde_yaml::Value::Number(serde_yaml::Number::from(*n)),
        Value::String(s) => serde_yaml::Value::String(s.clone()),
        Value::Seq(items) => serde_yaml::Value::Sequence(items.iter().map(to_serde).collect()),
        Value::Map(entries) => {
            let mut map = serde_yaml::Mapping::with_capacity(entries.len());
            for (k, v) in entries {
                map.insert(to_serde(k), to_serde(v));
            }
            serde_yaml::Value::Mapping(map)
        }
    }
}

use serde::Deserialize;

/// Parses `yaml_text` as a single YAML document and renders it as
/// JSON. The shape mirrors `encoding::toml::to_json` so callers can
/// chain into `json::parse` or auto-derived `<Type>::from_yaml`.
pub fn to_json(yaml_text: &str) -> Result<String, String> {
    let v = parse(yaml_text).map_err(|e| e.message)?;
    let jv = value_to_json(&v);
    serde_json::to_string(&jv).map_err(|e| e.to_string())
}

/// Renders a JSON document as YAML. Round-trips through the dynamic
/// [`Value`] representation; YAML-only constructs (tags, multi-doc
/// anchors) are not produced.
pub fn from_json(json_text: &str) -> Result<String, String> {
    let jv: serde_json::Value = serde_json::from_str(json_text).map_err(|e| e.to_string())?;
    let yv = json_to_value(&jv);
    encode(&yv).map_err(|e| e.message)
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Seq(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::Map(entries) => {
            let mut map = serde_json::Map::new();
            for (k, v) in entries {
                let key = match k {
                    Value::String(s) => s.clone(),
                    Value::Int(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Null => "null".to_string(),
                    other => format!("{other:?}"),
                };
                map.insert(key, value_to_json(v));
            }
            serde_json::Value::Object(map)
        }
    }
}

fn json_to_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(items) => Value::Seq(items.iter().map(json_to_value).collect()),
        serde_json::Value::Object(map) => Value::Map(
            map.iter()
                .map(|(k, v)| (Value::String(k.clone()), json_to_value(v)))
                .collect(),
        ),
    }
}

/// Re-exposes [`Value`] as a `BTreeMap`-keyed view for callers that
/// only ever produce string-keyed maps. Returns `Err` when any key
/// is not a string.
pub fn into_object(value: Value) -> Result<BTreeMap<String, Value>, Error> {
    match value {
        Value::Map(entries) => {
            let mut out = BTreeMap::new();
            for (k, v) in entries {
                let key = match k {
                    Value::String(s) => s,
                    other => {
                        return Err(Error {
                            message: format!("non-string key in mapping: {other:?}"),
                        });
                    }
                };
                out.insert(key, v);
            }
            Ok(out)
        }
        other => Err(Error {
            message: format!("expected mapping, got {other:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_scalar() {
        assert_eq!(parse("42").unwrap(), Value::Int(42));
        assert_eq!(parse("true").unwrap(), Value::Bool(true));
        assert_eq!(parse("hello").unwrap(), Value::String("hello".into()));
    }

    #[test]
    fn parses_nested_map() {
        let doc = "name: gossamer\nversion: 1\ndeps:\n  - a\n  - b\n";
        let parsed = parse(doc).unwrap();
        let name = parsed.get("name").unwrap();
        assert_eq!(name.as_str(), Some("gossamer"));
        let version = parsed.get("version").unwrap();
        assert_eq!(version.as_i64(), Some(1));
        let deps = parsed.get("deps").unwrap().as_seq().unwrap();
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].as_str(), Some("a"));
    }

    #[test]
    fn parse_all_roundtrip() {
        let stream = "---\nfoo: 1\n---\nbar: 2\n";
        let docs = parse_all(stream).unwrap();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].get("foo").unwrap().as_i64(), Some(1));
        assert_eq!(docs[1].get("bar").unwrap().as_i64(), Some(2));
    }

    #[test]
    fn encode_roundtrip() {
        let value = Value::Map(vec![
            (Value::String("a".into()), Value::Int(1)),
            (Value::String("b".into()), Value::String("two".into())),
        ]);
        let text = encode(&value).unwrap();
        let back = parse(&text).unwrap();
        assert_eq!(back.get("a").unwrap().as_i64(), Some(1));
        assert_eq!(back.get("b").unwrap().as_str(), Some("two"));
    }

    #[test]
    fn yaml_to_json_round_trip() {
        let yaml = "name: gossamer\nversion: 7\nflags:\n  - fast\n  - small\n";
        let json = to_json(yaml).unwrap();
        assert!(json.contains("\"name\":\"gossamer\""));
        assert!(json.contains("\"version\":7"));
        assert!(json.contains("\"flags\":[\"fast\",\"small\"]"));
    }

    #[test]
    fn yaml_from_json_round_trip() {
        let json = r#"{"name":"gossamer","port":8080}"#;
        let yaml = from_json(json).unwrap();
        assert!(yaml.contains("name: gossamer"));
        assert!(yaml.contains("port: 8080"));
    }
}
