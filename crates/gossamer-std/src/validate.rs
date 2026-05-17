//! Runtime support for `std::validate`.
//!
//! Field-level validation framework: `Validate` trait, `Errors`
//! collection, and a set of standalone rule helpers that the
//! autoderive pass (or user code) composes into a `validate()`
//! implementation.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;

use crate::errors::Error;

/// A single field-level validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    /// Machine-readable rule identifier (e.g. `"length"`, `"email"`).
    pub code: String,
    /// Human-readable failure message.
    pub message: String,
    /// Rule-specific parameters as `(name, rendered_value)` pairs.
    pub params: Vec<(String, String)>,
}

impl FieldError {
    /// Constructs a `FieldError` from owned parts.
    #[must_use]
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        params: Vec<(String, String)>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            params,
        }
    }
}

/// All validation failures for one struct, keyed by field path.
#[derive(Debug, Clone, Default)]
pub struct Errors {
    fields: BTreeMap<String, Vec<FieldError>>,
}

impl Errors {
    /// Constructs an empty error set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true when no failures have been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Returns the total number of recorded `FieldError`s across
    /// every field.
    #[must_use]
    pub fn len(&self) -> usize {
        self.fields.values().map(Vec::len).sum()
    }

    /// Appends `err` to the list of failures for `field`.
    pub fn add(&mut self, field: impl Into<String>, err: FieldError) {
        self.fields.entry(field.into()).or_default().push(err);
    }

    /// Merges `other` into `self`, prefixing every field key with
    /// `prefix + "."`. Intended for nested struct validation, e.g.
    /// `errs.merge_with_prefix("address", inner_errs)`.
    pub fn merge_with_prefix(&mut self, prefix: &str, other: Errors) {
        for (field, errs) in other.fields {
            let path = if prefix.is_empty() {
                field
            } else {
                format!("{prefix}.{field}")
            };
            self.fields.entry(path).or_default().extend(errs);
        }
    }

    /// Returns the slice of failures recorded for `field`, if any.
    #[must_use]
    pub fn get(&self, field: &str) -> Option<&[FieldError]> {
        self.fields.get(field).map(Vec::as_slice)
    }

    /// Iterates `(field, &[FieldError])` pairs in sorted-key order.
    pub fn fields(&self) -> impl Iterator<Item = (&str, &[FieldError])> {
        self.fields.iter().map(|(k, v)| (k.as_str(), v.as_slice()))
    }

    /// Renders as a JSON object: `{field: [{code, message, params}, ...]}`.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let mut root = serde_json::Map::new();
        for (field, errs) in &self.fields {
            let arr: Vec<serde_json::Value> = errs
                .iter()
                .map(|e| {
                    let mut params = serde_json::Map::new();
                    for (k, v) in &e.params {
                        params.insert(k.clone(), serde_json::Value::String(v.clone()));
                    }
                    let mut obj = serde_json::Map::new();
                    obj.insert(
                        "code".to_string(),
                        serde_json::Value::String(e.code.clone()),
                    );
                    obj.insert(
                        "message".to_string(),
                        serde_json::Value::String(e.message.clone()),
                    );
                    obj.insert("params".to_string(), serde_json::Value::Object(params));
                    serde_json::Value::Object(obj)
                })
                .collect();
            root.insert(field.clone(), serde_json::Value::Array(arr));
        }
        serde_json::Value::Object(root)
    }

    /// Collapses the set into a single `errors::Error` for `?`
    /// propagation. The message lists every failure as
    /// `field: message` joined with `"; "`.
    #[must_use]
    pub fn into_error(self) -> Error {
        Error::new(self.to_string())
    }
}

impl fmt::Display for Errors {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (field, errs) in &self.fields {
            for e in errs {
                if !first {
                    out.write_str("; ")?;
                }
                first = false;
                write!(out, "{field}: {}", e.message)?;
            }
        }
        Ok(())
    }
}

/// Trait implemented by every validatable user type.
///
/// The autoderive pass synthesises this from `#[validate(...)]`
/// attributes; users may also implement it by hand.
pub trait Validate {
    /// Runs every field rule and returns the accumulated failures.
    ///
    /// # Errors
    ///
    /// Returns `Err(Errors)` when any field rule fails.
    fn validate(&self) -> Result<(), Errors>;
}

/// Standalone rule helpers callable from `Validate` impls or
/// autoderived code.
pub mod rules {
    use super::FieldError;
    use regex::Regex;

    fn param(name: &str, value: impl ToString) -> (String, String) {
        (name.to_string(), value.to_string())
    }

    /// Fails when `value` is empty or whitespace-only.
    ///
    /// # Errors
    ///
    /// Returns a `required` `FieldError` if the trimmed value is empty.
    pub fn required_str(value: &str) -> Result<(), FieldError> {
        if value.trim().is_empty() {
            return Err(FieldError::new("required", "must not be empty", Vec::new()));
        }
        Ok(())
    }

    /// Fails when `value`'s character length is outside `[min, max]`.
    ///
    /// Either bound may be `None`. Length is counted in Unicode
    /// scalar values (chars), not bytes.
    ///
    /// # Errors
    ///
    /// Returns a `length` `FieldError` when the value is too short or
    /// too long.
    pub fn length(value: &str, min: Option<usize>, max: Option<usize>) -> Result<(), FieldError> {
        let n = value.chars().count();
        let mut params = Vec::new();
        if let Some(m) = min {
            params.push(param("min", m));
        }
        if let Some(m) = max {
            params.push(param("max", m));
        }
        if let Some(m) = min {
            if n < m {
                return Err(FieldError::new(
                    "length",
                    format!("must be at least {m} characters"),
                    params,
                ));
            }
        }
        if let Some(m) = max {
            if n > m {
                return Err(FieldError::new(
                    "length",
                    format!("must be at most {m} characters"),
                    params,
                ));
            }
        }
        Ok(())
    }

    /// Fails when `value` is outside `[min, max]` (inclusive).
    ///
    /// # Errors
    ///
    /// Returns a `range` `FieldError` when the value falls outside the
    /// supplied bounds.
    pub fn range_i64(value: i64, min: Option<i64>, max: Option<i64>) -> Result<(), FieldError> {
        let mut params = Vec::new();
        if let Some(m) = min {
            params.push(param("min", m));
        }
        if let Some(m) = max {
            params.push(param("max", m));
        }
        if let Some(m) = min {
            if value < m {
                return Err(FieldError::new("range", format!("must be >= {m}"), params));
            }
        }
        if let Some(m) = max {
            if value > m {
                return Err(FieldError::new("range", format!("must be <= {m}"), params));
            }
        }
        Ok(())
    }

    /// Fails when `value` is outside `[min, max]` (inclusive).
    ///
    /// NaN compares false to every bound and therefore always fails
    /// when any bound is supplied.
    ///
    /// # Errors
    ///
    /// Returns a `range` `FieldError` when the value falls outside the
    /// supplied bounds.
    pub fn range_f64(value: f64, min: Option<f64>, max: Option<f64>) -> Result<(), FieldError> {
        let mut params = Vec::new();
        if let Some(m) = min {
            params.push(param("min", m));
        }
        if let Some(m) = max {
            params.push(param("max", m));
        }
        // Negated comparisons make NaN fail every bound (NaN >= x is false).
        #[allow(
            clippy::neg_cmp_op_on_partial_ord,
            reason = "NaN must fail bound checks; rewriting as < would treat NaN as in-range"
        )]
        if let Some(m) = min {
            if !(value >= m) {
                return Err(FieldError::new("range", format!("must be >= {m}"), params));
            }
        }
        #[allow(
            clippy::neg_cmp_op_on_partial_ord,
            reason = "NaN must fail bound checks; rewriting as > would treat NaN as in-range"
        )]
        if let Some(m) = max {
            if !(value <= m) {
                return Err(FieldError::new("range", format!("must be <= {m}"), params));
            }
        }
        Ok(())
    }

    /// Pragmatic email validator. See module docs for the exact rules.
    ///
    /// # Errors
    ///
    /// Returns an `email` `FieldError` when the value does not look
    /// like a plausible address.
    pub fn email(value: &str) -> Result<(), FieldError> {
        let fail = || FieldError::new("email", "must be a valid email", Vec::new());
        if value.chars().any(char::is_whitespace) {
            return Err(fail());
        }
        let mut parts = value.split('@');
        let (local, domain, extra) = (parts.next(), parts.next(), parts.next());
        let (Some(local), Some(domain)) = (local, domain) else {
            return Err(fail());
        };
        if extra.is_some() {
            return Err(fail());
        }
        if local.is_empty() || domain.is_empty() {
            return Err(fail());
        }
        if !domain.contains('.') {
            return Err(fail());
        }
        if domain.starts_with('.') || domain.ends_with('.') {
            return Err(fail());
        }
        Ok(())
    }

    /// Pragmatic URL validator. See module docs for the exact rules.
    ///
    /// # Errors
    ///
    /// Returns a `url` `FieldError` when the value lacks a scheme or
    /// host.
    pub fn url(value: &str) -> Result<(), FieldError> {
        let fail = || FieldError::new("url", "must be a valid URL", Vec::new());
        if value.chars().any(char::is_whitespace) {
            return Err(fail());
        }
        let Some(idx) = value.find("://") else {
            return Err(fail());
        };
        let scheme = &value[..idx];
        if scheme.is_empty() {
            return Err(fail());
        }
        let valid_scheme = scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.');
        if !valid_scheme {
            return Err(fail());
        }
        let rest = &value[idx + 3..];
        let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let host = &rest[..host_end];
        if host.is_empty() {
            return Err(fail());
        }
        Ok(())
    }

    /// Validates `value` against a regular expression `pattern`.
    ///
    /// On compile failure, returns a `regex_invalid` `FieldError`
    /// containing the parser error message. On match failure, returns
    /// a `regex` `FieldError`.
    ///
    /// # Errors
    ///
    /// Returns an error when the pattern is invalid or the value does
    /// not match.
    pub fn regex(value: &str, pattern: &str) -> Result<(), FieldError> {
        match Regex::new(pattern) {
            Err(e) => Err(FieldError::new(
                "regex_invalid",
                e.to_string(),
                vec![param("pattern", pattern)],
            )),
            Ok(re) if re.is_match(value) => Ok(()),
            Ok(_) => Err(FieldError::new(
                "regex",
                format!("must match pattern {pattern}"),
                vec![param("pattern", pattern)],
            )),
        }
    }

    /// Fails when `value` is not present in `allowed`.
    ///
    /// # Errors
    ///
    /// Returns a `one_of` `FieldError` when the value is not in the
    /// allow-list.
    pub fn one_of<T>(value: &T, allowed: &[T]) -> Result<(), FieldError>
    where
        T: PartialEq + std::fmt::Display,
    {
        if allowed.iter().any(|a| a == value) {
            return Ok(());
        }
        let rendered = allowed
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        Err(FieldError::new(
            "one_of",
            format!("must be one of [{rendered}]"),
            vec![param("allowed", rendered)],
        ))
    }

    /// Fails when `value != expected`.
    ///
    /// # Errors
    ///
    /// Returns an `eq` `FieldError` on inequality.
    pub fn eq<T>(value: &T, expected: &T) -> Result<(), FieldError>
    where
        T: PartialEq + std::fmt::Display,
    {
        if value == expected {
            Ok(())
        } else {
            Err(FieldError::new(
                "eq",
                format!("must equal {expected}"),
                vec![param("expected", expected)],
            ))
        }
    }

    /// Fails when `value == forbidden`.
    ///
    /// # Errors
    ///
    /// Returns a `ne` `FieldError` on equality.
    pub fn ne<T>(value: &T, forbidden: &T) -> Result<(), FieldError>
    where
        T: PartialEq + std::fmt::Display,
    {
        if value == forbidden {
            Err(FieldError::new(
                "ne",
                format!("must not equal {forbidden}"),
                vec![param("forbidden", forbidden)],
            ))
        } else {
            Ok(())
        }
    }

    /// Fails when `value` does not start with `prefix`.
    ///
    /// # Errors
    ///
    /// Returns a `starts_with` `FieldError` on mismatch.
    pub fn starts_with(value: &str, prefix: &str) -> Result<(), FieldError> {
        if value.starts_with(prefix) {
            Ok(())
        } else {
            Err(FieldError::new(
                "starts_with",
                format!("must start with {prefix:?}"),
                vec![param("prefix", prefix)],
            ))
        }
    }

    /// Fails when `value` does not end with `suffix`.
    ///
    /// # Errors
    ///
    /// Returns an `ends_with` `FieldError` on mismatch.
    pub fn ends_with(value: &str, suffix: &str) -> Result<(), FieldError> {
        if value.ends_with(suffix) {
            Ok(())
        } else {
            Err(FieldError::new(
                "ends_with",
                format!("must end with {suffix:?}"),
                vec![param("suffix", suffix)],
            ))
        }
    }

    /// Fails when `value` does not contain `needle`.
    ///
    /// # Errors
    ///
    /// Returns a `contains` `FieldError` on absence.
    pub fn contains(value: &str, needle: &str) -> Result<(), FieldError> {
        if value.contains(needle) {
            Ok(())
        } else {
            Err(FieldError::new(
                "contains",
                format!("must contain {needle:?}"),
                vec![param("needle", needle)],
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::rules::*;
    use super::*;

    #[test]
    fn required_str_rejects_empty_and_whitespace() {
        assert!(required_str("").is_err());
        assert!(required_str("   \t\n").is_err());
        assert!(required_str("ok").is_ok());
    }

    #[test]
    fn length_enforces_min_and_max() {
        assert!(length("ab", Some(3), None).is_err());
        assert!(length("abc", Some(3), None).is_ok());
        assert!(length("abcd", None, Some(3)).is_err());
        assert!(length("abc", None, Some(3)).is_ok());
        assert!(length("hello", Some(1), Some(10)).is_ok());
        let many_chars = "naïve";
        assert!(length(many_chars, Some(5), Some(5)).is_ok());
    }

    #[test]
    fn range_i64_enforces_bounds() {
        assert!(range_i64(5, Some(10), None).is_err());
        assert!(range_i64(10, Some(10), None).is_ok());
        assert!(range_i64(11, None, Some(10)).is_err());
        assert!(range_i64(10, None, Some(10)).is_ok());
        assert!(range_i64(5, Some(1), Some(10)).is_ok());
    }

    #[test]
    fn range_f64_enforces_bounds() {
        assert!(range_f64(0.5, Some(1.0), None).is_err());
        assert!(range_f64(1.0, Some(1.0), None).is_ok());
        assert!(range_f64(2.5, None, Some(2.0)).is_err());
        assert!(range_f64(2.0, None, Some(2.0)).is_ok());
        assert!(range_f64(f64::NAN, Some(0.0), Some(1.0)).is_err());
    }

    #[test]
    fn email_accepts_plausible_addresses() {
        assert!(email("user@example.com").is_ok());
        assert!(email("a.b+tag@sub.example.co.uk").is_ok());
        assert!(email("x@y.z").is_ok());
    }

    #[test]
    fn email_rejects_obvious_garbage() {
        assert!(email("no-at-sign").is_err());
        assert!(email("two@@signs.com").is_err());
        assert!(email("nodot@nodomain").is_err());
        assert!(email("white space@example.com").is_err());
        assert!(email("@nodomain.com").is_err());
        assert!(email("noLocal@").is_err());
        assert!(email("trailing@dot.").is_err());
    }

    #[test]
    fn url_accepts_plausible_urls() {
        assert!(url("http://example.com").is_ok());
        assert!(url("https://example.com/path?q=1").is_ok());
        assert!(url("ftp://files.example.org/dir/").is_ok());
    }

    #[test]
    fn url_rejects_obvious_garbage() {
        assert!(url("example.com").is_err());
        assert!(url("http://").is_err());
        assert!(url("://nohost.com").is_err());
        assert!(url("http:// space.com").is_err());
    }

    #[test]
    fn regex_matches_and_compile_errors() {
        assert!(regex("abc123", r"^[a-z]+\d+$").is_ok());
        let fail = regex("XYZ", r"^[a-z]+$").unwrap_err();
        assert_eq!(fail.code, "regex");
        let bad = regex("anything", r"(unclosed").unwrap_err();
        assert_eq!(bad.code, "regex_invalid");
    }

    #[test]
    fn one_of_eq_ne_helpers() {
        assert!(
            one_of(
                &"red".to_string(),
                &["red".to_string(), "green".to_string()]
            )
            .is_ok()
        );
        assert!(
            one_of(
                &"blue".to_string(),
                &["red".to_string(), "green".to_string()]
            )
            .is_err()
        );
        assert!(eq(&3_i64, &3).is_ok());
        assert!(eq(&3_i64, &4).is_err());
        assert!(ne(&3_i64, &4).is_ok());
        assert!(ne(&3_i64, &3).is_err());
    }

    #[test]
    fn substring_helpers() {
        assert!(starts_with("hello world", "hello").is_ok());
        assert!(starts_with("hello world", "world").is_err());
        assert!(ends_with("hello world", "world").is_ok());
        assert!(ends_with("hello world", "hello").is_err());
        assert!(contains("hello world", "lo wo").is_ok());
        assert!(contains("hello world", "xyz").is_err());
    }

    #[test]
    fn errors_collects_per_field() {
        let mut errs = Errors::new();
        assert!(errs.is_empty());
        errs.add(
            "name",
            FieldError::new("required", "must not be empty", vec![]),
        );
        errs.add(
            "age",
            FieldError::new("range", "must be >= 0", vec![("min".into(), "0".into())]),
        );
        errs.add("name", FieldError::new("length", "too short", vec![]));
        assert!(!errs.is_empty());
        assert_eq!(errs.len(), 3);
        assert_eq!(errs.get("name").map(<[_]>::len), Some(2));
        assert_eq!(errs.get("missing"), None);
    }

    #[test]
    fn errors_to_json_shape() {
        let mut errs = Errors::new();
        errs.add(
            "name",
            FieldError::new("required", "must not be empty", vec![]),
        );
        errs.add(
            "age",
            FieldError::new(
                "range",
                "must be >= 0",
                vec![("min".into(), "0".into()), ("max".into(), "120".into())],
            ),
        );
        let json = errs.to_json();
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("name"));
        assert!(obj.contains_key("age"));
        let age_arr = obj["age"].as_array().unwrap();
        assert_eq!(age_arr.len(), 1);
        let e0 = &age_arr[0];
        assert_eq!(e0["code"], "range");
        assert_eq!(e0["message"], "must be >= 0");
        assert_eq!(e0["params"]["min"], "0");
        assert_eq!(e0["params"]["max"], "120");
    }

    #[test]
    fn errors_merge_with_prefix_namespaces_fields() {
        let mut outer = Errors::new();
        outer.add(
            "name",
            FieldError::new("required", "must not be empty", vec![]),
        );
        let mut inner = Errors::new();
        inner.add(
            "city",
            FieldError::new("required", "must not be empty", vec![]),
        );
        inner.add("zip", FieldError::new("length", "too short", vec![]));
        outer.merge_with_prefix("address", inner);
        assert_eq!(outer.len(), 3);
        assert!(outer.get("address.city").is_some());
        assert!(outer.get("address.zip").is_some());
        assert!(outer.get("name").is_some());
    }

    #[test]
    fn errors_merge_with_empty_prefix_keeps_original_keys() {
        let mut outer = Errors::new();
        let mut inner = Errors::new();
        inner.add(
            "x",
            FieldError::new("required", "must not be empty", vec![]),
        );
        outer.merge_with_prefix("", inner);
        assert!(outer.get("x").is_some());
    }

    #[test]
    fn errors_display_and_into_error() {
        let mut errs = Errors::new();
        errs.add(
            "name",
            FieldError::new("required", "must not be empty", vec![]),
        );
        let rendered = errs.to_string();
        assert!(rendered.contains("name:"));
        let err = errs.into_error();
        assert!(err.message().contains("name:"));
    }

    struct Signup {
        name: String,
        email_addr: String,
        age: i64,
    }

    impl Validate for Signup {
        fn validate(&self) -> Result<(), Errors> {
            let mut errs = Errors::new();
            if let Err(e) = required_str(&self.name) {
                errs.add("name", e);
            }
            if let Err(e) = length(&self.name, Some(2), Some(64)) {
                errs.add("name", e);
            }
            if let Err(e) = email(&self.email_addr) {
                errs.add("email_addr", e);
            }
            if let Err(e) = range_i64(self.age, Some(13), Some(120)) {
                errs.add("age", e);
            }
            if errs.is_empty() { Ok(()) } else { Err(errs) }
        }
    }

    #[test]
    fn validate_impl_collects_multiple_failures() {
        let bad = Signup {
            name: "x".to_string(),
            email_addr: "not-an-email".to_string(),
            age: 5,
        };
        let errs = bad.validate().unwrap_err();
        assert_eq!(errs.len(), 3);
        assert!(errs.get("name").is_some());
        assert!(errs.get("email_addr").is_some());
        assert!(errs.get("age").is_some());

        let good = Signup {
            name: "ada".to_string(),
            email_addr: "ada@example.com".to_string(),
            age: 30,
        };
        assert!(good.validate().is_ok());
    }

    #[test]
    fn iter_fields_visits_each_recorded_field() {
        let mut errs = Errors::new();
        errs.add("a", FieldError::new("x", "m1", vec![]));
        errs.add("b", FieldError::new("y", "m2", vec![]));
        let collected: Vec<&str> = errs.fields().map(|(k, _)| k).collect();
        assert_eq!(collected, vec!["a", "b"]);
    }
}
