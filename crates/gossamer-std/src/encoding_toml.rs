// `std::encoding::toml` - TOML 1.0 parsing + emission.
//
// Surface stays simple to keep the cross-tier C-ABI light:
//   - `to_json(toml_text)`   -> Result<json_text, errors::Error>
//   - `from_json(json_text)` -> Result<toml_text, errors::Error>
//   - `is_valid(s)`          -> bool
//   - `pretty(s)`            -> Result<toml_text, errors::Error>
//
// For typed deserialization, every user struct gets a
// `<Type>::from_toml` / `to_toml` derive at `Vm::load` (extends the
// JSON auto-derive pattern; routes through `to_json` / `from_json`
// internally - same schema registry).

#![forbid(unsafe_code)]

/// Parses `text` as TOML and returns its JSON-shaped serialization.
/// JSON is the lingua franca for downstream consumers - chain into
/// `json::parse` or use the auto-derived `<Type>::from_toml`.
pub fn to_json(text: &str) -> Result<String, String> {
    let value: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
    let json = toml_value_to_json(&value);
    serde_json::to_string(&json).map_err(|e| e.to_string())
}

/// Renders a JSON document as TOML. Top-level must be an object;
/// arrays-of-objects become `[[name]]` table arrays.
pub fn from_json(json_text: &str) -> Result<String, String> {
    let v: serde_json::Value = serde_json::from_str(json_text).map_err(|e| e.to_string())?;
    let toml_value = json_to_toml_value(&v)?;
    toml::to_string_pretty(&toml_value).map_err(|e| e.to_string())
}

/// `true` iff `text` parses as TOML.
#[must_use]
pub fn is_valid(text: &str) -> bool {
    text.parse::<toml::Value>().is_ok()
}

/// Round-trips `text` through the parser + pretty-printer.
pub fn pretty(text: &str) -> Result<String, String> {
    let value: toml::Value = toml::from_str(text).map_err(|e| e.to_string())?;
    toml::to_string_pretty(&value).map_err(|e| e.to_string())
}

fn toml_value_to_json(v: &toml::Value) -> serde_json::Value {
    match v {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::Value::Number((*i).into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
        toml::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(toml_value_to_json).collect())
        }
        toml::Value::Table(t) => {
            let mut map = serde_json::Map::new();
            for (k, v) in t {
                map.insert(k.clone(), toml_value_to_json(v));
            }
            serde_json::Value::Object(map)
        }
    }
}

fn json_to_toml_value(v: &serde_json::Value) -> Result<toml::Value, String> {
    match v {
        serde_json::Value::Null => Err("TOML has no null".to_string()),
        serde_json::Value::Bool(b) => Ok(toml::Value::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(toml::Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(toml::Value::Float(f))
            } else {
                Err(format!("unrepresentable number: {n}"))
            }
        }
        serde_json::Value::String(s) => Ok(toml::Value::String(s.clone())),
        serde_json::Value::Array(items) => Ok(toml::Value::Array(
            items
                .iter()
                .map(json_to_toml_value)
                .collect::<Result<_, _>>()?,
        )),
        serde_json::Value::Object(map) => {
            let mut t = toml::value::Table::new();
            for (k, v) in map {
                t.insert(k.clone(), json_to_toml_value(v)?);
            }
            Ok(toml::Value::Table(t))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_json_round_trip_simple() {
        let toml_text = r#"
            port = 8080
            host = "localhost"
            verbose = true
        "#;
        let json = to_json(toml_text).unwrap();
        assert!(json.contains("\"port\":8080"));
        assert!(json.contains("\"host\":\"localhost\""));
        assert!(json.contains("\"verbose\":true"));
    }

    #[test]
    fn is_valid_accepts_canonical() {
        assert!(is_valid("port = 8080"));
    }

    #[test]
    fn is_valid_rejects_garbage() {
        assert!(!is_valid("port == 8080"));
    }

    #[test]
    fn from_json_round_trip() {
        let json = r#"{"port":8080,"host":"localhost"}"#;
        let toml_text = from_json(json).unwrap();
        assert!(toml_text.contains("port = 8080"));
        assert!(toml_text.contains("host = \"localhost\""));
    }

    #[test]
    fn pretty_normalizes() {
        let messy = "host =    \"localhost\"\nport=8080\n";
        let clean = pretty(messy).unwrap();
        // Pretty-printer normalizes spacing; both keys remain present.
        assert!(clean.contains("host"));
        assert!(clean.contains("port"));
    }
}
