#![allow(
    clippy::similar_names,
    clippy::map_unwrap_or,
    clippy::missing_errors_doc,
    clippy::must_use_candidate,
    clippy::uninlined_format_args
)]
#![forbid(unsafe_code)]

//! Typed query-string wrapper.
//!
//! `Query` parses a URL query string (the part after `?`, with
//! the `?` already stripped) using the same wire format as
//! `application/x-www-form-urlencoded`: `key=value` pairs joined
//! by `&`, with `+` for spaces and `%XX` for arbitrary bytes
//! (HTML4 + RFC 3986 §3.4).
//!
//! Conceptually a query string and a form body are *not* the same:
//! one travels in the URL line, the other in the request body.
//! Keeping them as distinct types (rather than aliasing
//! `Query = Form`) lets future extractors (`Query<T>`,
//! `Form<T>`) carry independent semantics and lets callsites
//! self-document which surface they speak to.
//!
//! Parsing delegates to [`crate::url::decode_query`]; serialization
//! delegates to [`crate::url::encode_query`].

use crate::errors::Error;

/// Decoded query string: ordered `(name, value)` pairs.
pub struct Query {
    pairs: Vec<(String, String)>,
}

impl Query {
    /// Parses a query string. The caller is expected to strip
    /// any leading `?` first; an empty input yields an empty
    /// query.
    pub fn parse(raw: &str) -> Result<Self, Error> {
        let pairs = crate::url::decode_query(raw)?;
        Ok(Self { pairs })
    }

    /// Returns an empty query.
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

    /// Reports whether the query has no pairs.
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// Parses the first value for `name` as `i64`.
    pub fn get_i64(&self, name: &str) -> Result<i64, Error> {
        let raw = self
            .get(name)
            .ok_or_else(|| Error::new(format!("query field `{}` missing", name)))?;
        raw.parse::<i64>()
            .map_err(|_| Error::new(format!("query field `{}`: not an i64: {:?}", name, raw)))
    }

    /// Parses the first value for `name` as `f64`.
    pub fn get_f64(&self, name: &str) -> Result<f64, Error> {
        let raw = self
            .get(name)
            .ok_or_else(|| Error::new(format!("query field `{}` missing", name)))?;
        raw.parse::<f64>()
            .map_err(|_| Error::new(format!("query field `{}`: not an f64: {:?}", name, raw)))
    }

    /// Parses the first value for `name` as `bool`.
    ///
    /// Accepts `true`/`1`/`on`/`yes` as true and
    /// `false`/`0`/`off`/`no`/empty as false. Comparison is
    /// case-insensitive on ASCII; all other spellings error.
    pub fn get_bool(&self, name: &str) -> Result<bool, Error> {
        let raw = self
            .get(name)
            .ok_or_else(|| Error::new(format!("query field `{}` missing", name)))?;
        parse_query_bool(name, raw)
    }

    /// Serializes back to a query string (no leading `?`).
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

fn parse_query_bool(name: &str, raw: &str) -> Result<bool, Error> {
    let lower = raw.to_ascii_lowercase();
    match lower.as_str() {
        "true" | "1" | "on" | "yes" => Ok(true),
        "false" | "0" | "off" | "no" | "" => Ok(false),
        _ => Err(Error::new(format!(
            "query field `{}`: not a bool: {:?}",
            name, raw
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_query() {
        let q = Query::parse("").unwrap();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
        assert_eq!(q.get("anything"), None);
    }

    #[test]
    fn parse_single_pair() {
        let q = Query::parse("page=2").unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q.get("page"), Some("2"));
        assert!(q.contains("page"));
        assert!(!q.contains("missing"));
    }

    #[test]
    fn parse_multi_pair_preserves_order() {
        let q = Query::parse("a=1&b=2&c=3").unwrap();
        let collected: Vec<(&str, &str)> = q.iter().collect();
        assert_eq!(collected, vec![("a", "1"), ("b", "2"), ("c", "3")]);
    }

    #[test]
    fn get_all_returns_repeats_in_order() {
        let q = Query::parse("id=1&id=2&id=3&other=x").unwrap();
        assert_eq!(q.get("id"), Some("1"));
        assert_eq!(q.get_all("id"), vec!["1", "2", "3"]);
        assert_eq!(q.get_all("missing"), Vec::<&str>::new());
    }

    #[test]
    fn percent_and_plus_decoded() {
        let q = Query::parse("name=jane+doe&age=30&city=New%20York").unwrap();
        assert_eq!(q.get("name"), Some("jane doe"));
        assert_eq!(q.get("age"), Some("30"));
        assert_eq!(q.get("city"), Some("New York"));
    }

    #[test]
    fn get_i64_happy_and_error() {
        let q = Query::parse("page=5&label=hi").unwrap();
        assert_eq!(q.get_i64("page").unwrap(), 5);
        let missing = q.get_i64("absent").unwrap_err();
        assert!(format!("{}", missing).contains("missing"));
        let bad = q.get_i64("label").unwrap_err();
        assert!(format!("{}", bad).contains("not an i64"));
    }

    #[test]
    fn get_f64_happy_and_error() {
        let q = Query::parse("weight=2.5&label=nope").unwrap();
        assert!((q.get_f64("weight").unwrap() - 2.5).abs() < 1e-9);
        assert!(q.get_f64("label").is_err());
    }

    #[test]
    fn get_bool_truthy_falsy_variants() {
        let truthy = Query::parse("a=true&b=1&c=on&d=YES&e=True").unwrap();
        for k in ["a", "b", "c", "d", "e"] {
            assert!(truthy.get_bool(k).unwrap(), "key `{k}` should be true");
        }
        let falsy = Query::parse("a=false&b=0&c=off&d=no&e=").unwrap();
        for k in ["a", "b", "c", "d", "e"] {
            assert!(!falsy.get_bool(k).unwrap(), "key `{k}` should be false");
        }
        let bad = Query::parse("flag=maybe").unwrap();
        assert!(bad.get_bool("flag").is_err());
    }

    #[test]
    fn to_string_round_trip() {
        let original = "q=hello+world&page=2&filter=New+York";
        let q = Query::parse(original).unwrap();
        let rendered = q.to_string();
        let again = Query::parse(&rendered).unwrap();
        assert_eq!(again.get("q"), Some("hello world"));
        assert_eq!(again.get("page"), Some("2"));
        assert_eq!(again.get("filter"), Some("New York"));
    }

    #[test]
    fn empty_constructor_is_empty() {
        let q = Query::empty();
        assert!(q.is_empty());
        assert_eq!(q.to_string(), "");
    }
}
