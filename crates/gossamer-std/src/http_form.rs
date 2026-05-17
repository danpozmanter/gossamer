#![allow(
    clippy::similar_names,
    clippy::map_unwrap_or,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::uninlined_format_args
)]
#![forbid(unsafe_code)]

//! `application/x-www-form-urlencoded` parser and serializer.
//!
//! Backs `std::http::form`. A [`Form`] is an ordered list of
//! `(name, value)` pairs decoded from a request body using the
//! HTML form-encoding rules (`+` -> space, `%XX` -> byte). The
//! same wire format is used for URL query strings, but those are
//! exposed through the sibling [`crate::http_query`] module so
//! callsites can distinguish "I parsed a body" from "I parsed
//! a query".
//!
//! Parsing delegates to [`crate::url::decode_query`]; serialization
//! delegates to [`crate::url::encode_query`].

use crate::errors::Error;

/// Decoded form body: ordered `(name, value)` pairs.
pub struct Form {
    pairs: Vec<(String, String)>,
}

impl Form {
    /// Parses an `application/x-www-form-urlencoded` body.
    pub fn parse(body: &str) -> Result<Self, Error> {
        let pairs = crate::url::decode_query(body)?;
        Ok(Self { pairs })
    }

    /// Returns an empty form.
    pub fn empty() -> Self {
        Self { pairs: Vec::new() }
    }

    /// Returns the first value bound to `name`, if any.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Returns every value bound to `name`, in source order.
    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.pairs
            .iter()
            .filter(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// Reports whether `name` appears at least once.
    pub fn contains(&self, name: &str) -> bool {
        self.pairs.iter().any(|(k, _)| k == name)
    }

    /// Iterates over the `(name, value)` pairs in source order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.pairs.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Number of pairs.
    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    /// Reports whether the form has no pairs.
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// Parses the first value for `name` as `i64`.
    pub fn get_i64(&self, name: &str) -> Result<i64, Error> {
        let raw = self
            .get(name)
            .ok_or_else(|| Error::new(format!("form field `{}` missing", name)))?;
        raw.parse::<i64>()
            .map_err(|_| Error::new(format!("form field `{}`: not an i64: {:?}", name, raw)))
    }

    /// Parses the first value for `name` as `f64`.
    pub fn get_f64(&self, name: &str) -> Result<f64, Error> {
        let raw = self
            .get(name)
            .ok_or_else(|| Error::new(format!("form field `{}` missing", name)))?;
        raw.parse::<f64>()
            .map_err(|_| Error::new(format!("form field `{}`: not an f64: {:?}", name, raw)))
    }

    /// Parses the first value for `name` as `bool`.
    ///
    /// Accepts `true`/`1`/`on`/`yes` as true and
    /// `false`/`0`/`off`/`no`/empty as false. Comparison is
    /// case-insensitive on ASCII; all other spellings error.
    pub fn get_bool(&self, name: &str) -> Result<bool, Error> {
        let raw = self
            .get(name)
            .ok_or_else(|| Error::new(format!("form field `{}` missing", name)))?;
        parse_form_bool(name, raw)
    }

    /// Serializes back to `application/x-www-form-urlencoded`.
    #[allow(clippy::inherent_to_string)]
    pub fn to_string(&self) -> String {
        let view: Vec<(&str, &str)> = self
            .pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        crate::url::encode_query(&view)
    }
}

/// Builder for assembling a [`Form`] programmatically.
pub struct FormBuilder {
    pairs: Vec<(String, String)>,
}

impl FormBuilder {
    /// Returns an empty builder.
    pub fn new() -> Self {
        Self { pairs: Vec::new() }
    }

    /// Appends a `(name, value)` pair. Repeats are preserved in
    /// insertion order — the same shape `Form::parse` produces.
    #[must_use]
    pub fn add(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.pairs.push((name.into(), value.into()));
        self
    }

    /// Consumes the builder and returns the built [`Form`].
    pub fn build(self) -> Form {
        Form { pairs: self.pairs }
    }
}

impl Default for FormBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// Shared boolean parser used by Form and Query.
pub(crate) fn parse_form_bool(name: &str, raw: &str) -> Result<bool, Error> {
    let lower = raw.to_ascii_lowercase();
    match lower.as_str() {
        "true" | "1" | "on" | "yes" => Ok(true),
        "false" | "0" | "off" | "no" | "" => Ok(false),
        _ => Err(Error::new(format!(
            "form field `{}`: not a bool: {:?}",
            name, raw
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_body() {
        let form = Form::parse("").unwrap();
        assert!(form.is_empty());
        assert_eq!(form.len(), 0);
        assert_eq!(form.get("anything"), None);
    }

    #[test]
    fn parse_single_pair() {
        let form = Form::parse("name=jane").unwrap();
        assert_eq!(form.len(), 1);
        assert_eq!(form.get("name"), Some("jane"));
        assert!(form.contains("name"));
        assert!(!form.contains("missing"));
    }

    #[test]
    fn parse_multi_pair_preserves_order() {
        let form = Form::parse("a=1&b=2&c=3").unwrap();
        let collected: Vec<(&str, &str)> = form.iter().collect();
        assert_eq!(collected, vec![("a", "1"), ("b", "2"), ("c", "3")]);
    }

    #[test]
    fn get_all_returns_repeats_in_order() {
        let form = Form::parse("tag=red&tag=green&tag=blue&other=x").unwrap();
        assert_eq!(form.get("tag"), Some("red"));
        assert_eq!(form.get_all("tag"), vec!["red", "green", "blue"]);
        assert_eq!(form.get_all("missing"), Vec::<&str>::new());
    }

    #[test]
    fn percent_and_plus_decoded() {
        let form = Form::parse("name=jane+doe&age=30&city=New%20York").unwrap();
        assert_eq!(form.get("name"), Some("jane doe"));
        assert_eq!(form.get("age"), Some("30"));
        assert_eq!(form.get("city"), Some("New York"));
    }

    #[test]
    fn get_i64_happy_and_error() {
        let form = Form::parse("count=42&label=hi").unwrap();
        assert_eq!(form.get_i64("count").unwrap(), 42);
        let missing = form.get_i64("absent").unwrap_err();
        assert!(format!("{}", missing).contains("missing"));
        let bad = form.get_i64("label").unwrap_err();
        assert!(format!("{}", bad).contains("not an i64"));
    }

    #[test]
    fn get_f64_happy_and_error() {
        let form = Form::parse("ratio=1.5&label=nope").unwrap();
        assert!((form.get_f64("ratio").unwrap() - 1.5).abs() < 1e-9);
        assert!(form.get_f64("label").is_err());
    }

    #[test]
    fn get_bool_truthy_falsy_variants() {
        let truthy = Form::parse("a=true&b=1&c=on&d=YES&e=True").unwrap();
        for k in ["a", "b", "c", "d", "e"] {
            assert!(truthy.get_bool(k).unwrap(), "key `{k}` should be true");
        }
        let falsy = Form::parse("a=false&b=0&c=off&d=no&e=").unwrap();
        for k in ["a", "b", "c", "d", "e"] {
            assert!(!falsy.get_bool(k).unwrap(), "key `{k}` should be false");
        }
        let bad = Form::parse("flag=maybe").unwrap();
        assert!(bad.get_bool("flag").is_err());
    }

    #[test]
    fn to_string_round_trip() {
        let original = "name=jane+doe&age=30&city=New+York";
        let form = Form::parse(original).unwrap();
        let rendered = form.to_string();
        let again = Form::parse(&rendered).unwrap();
        assert_eq!(again.get("name"), Some("jane doe"));
        assert_eq!(again.get("age"), Some("30"));
        assert_eq!(again.get("city"), Some("New York"));
    }

    #[test]
    fn builder_constructs_form_and_serializes() {
        let form = FormBuilder::new()
            .add("name", "jane doe")
            .add("age", "30")
            .add("tag", "red")
            .add("tag", "green")
            .build();
        assert_eq!(form.len(), 4);
        assert_eq!(form.get_all("tag"), vec!["red", "green"]);
        let wire = form.to_string();
        let parsed = Form::parse(&wire).unwrap();
        assert_eq!(parsed.get("name"), Some("jane doe"));
        assert_eq!(parsed.get_all("tag"), vec!["red", "green"]);
    }

    #[test]
    fn empty_constructor_is_empty() {
        let form = Form::empty();
        assert!(form.is_empty());
        assert_eq!(form.to_string(), "");
    }
}
