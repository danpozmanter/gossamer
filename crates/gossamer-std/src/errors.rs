//! Runtime support for `std::errors`.
//! Mirrors Go's `errors` package shape on top of Gossamer's existing
//! `Result<T, E>` + `Error` trait: constructors, wrapping (`%w`-style),
//! chain traversal, and predicate helpers (`is`, `as`).

#![forbid(unsafe_code)]

use std::fmt;
use std::sync::Arc;

/// A boxed, reference-counted error value.
///
/// Cloning is O(1): the underlying payload is shared via `Arc`.
#[derive(Clone)]
pub struct Error {
    inner: Arc<dyn ErrorObj>,
}

/// Implementation detail of [`Error`] - erased payload + optional
/// cause chain.
trait ErrorObj: Send + Sync + 'static {
    fn message(&self) -> &str;
    fn cause(&self) -> Option<&Error>;
    fn debug(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result;
    fn fields(&self) -> &[(String, String)];
}

struct Simple {
    message: String,
    cause: Option<Error>,
    fields: Vec<(String, String)>,
}

impl ErrorObj for Simple {
    fn message(&self) -> &str {
        &self.message
    }

    fn cause(&self) -> Option<&Error> {
        self.cause.as_ref()
    }

    fn debug(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(&self.message)
    }

    fn fields(&self) -> &[(String, String)] {
        &self.fields
    }
}

impl Error {
    /// Constructs a new error from an owned message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(Simple {
                message: message.into(),
                cause: None,
                fields: Vec::new(),
            }),
        }
    }

    /// Wraps `cause` with a higher-level message.
    ///
    /// Chain traversal with `chain` walks `self -> cause -> ...`.
    #[must_use]
    pub fn wrap(cause: Error, message: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(Simple {
                message: message.into(),
                cause: Some(cause),
                fields: Vec::new(),
            }),
        }
    }

    /// Returns the top-level message.
    #[must_use]
    pub fn message(&self) -> &str {
        self.inner.message()
    }

    /// Returns the direct cause, if any.
    #[must_use]
    pub fn cause(&self) -> Option<&Error> {
        self.inner.cause()
    }

    /// Returns a copy of this error with a structured diagnostic field. Fields
    /// are intended for machine classification such as SQLSTATE, operation,
    /// or endpoint. Callers must redact secrets before attaching them.
    #[must_use]
    pub fn with_field(&self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let mut fields = self.inner.fields().to_vec();
        let key = key.into();
        let value = value.into();
        if let Some((_, current)) = fields.iter_mut().find(|(name, _)| *name == key) {
            *current = value;
        } else {
            fields.push((key, value));
        }
        Self {
            inner: Arc::new(Simple {
                message: self.message().to_string(),
                cause: self.cause().cloned(),
                fields,
            }),
        }
    }

    /// Returns the value associated with `key`, if present on this error.
    #[must_use]
    pub fn field(&self, key: &str) -> Option<&str> {
        self.inner
            .fields()
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// Returns structured diagnostic fields in insertion order.
    #[must_use]
    pub fn fields(&self) -> &[(String, String)] {
        self.inner.fields()
    }

    /// Walks the cause chain starting at `self` and returns `true` if
    /// any link's message equals `needle`.
    #[must_use]
    pub fn is(&self, needle: &str) -> bool {
        self.chain().any(|err| err.message() == needle)
    }

    /// Returns an iterator over `self` and every ancestor cause.
    #[must_use]
    pub fn chain(&self) -> Chain<'_> {
        Chain { next: Some(self) }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.debug(out)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(self.message())?;
        let mut cursor = self.cause();
        while let Some(err) = cursor {
            out.write_str(": ")?;
            out.write_str(err.message())?;
            cursor = err.cause();
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

/// Iterator over an error chain.
pub struct Chain<'a> {
    next: Option<&'a Error>,
}

impl<'a> Iterator for Chain<'a> {
    type Item = &'a Error;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.next?;
        self.next = current.cause();
        Some(current)
    }
}

/// `errors::join(iter)` - collects every error in `iter` into one
/// semicolon-separated error. Go's `errors.Join` equivalent. The
/// "; " separator matches the interp builtin and the compiled
/// tiers' `gos_rt_errors_join*` shims so joined messages render
/// identically on every tier.
#[must_use]
pub fn join<I: IntoIterator<Item = Error>>(iter: I) -> Option<Error> {
    let mut parts = Vec::new();
    for err in iter {
        parts.push(err.message().to_string());
    }
    if parts.is_empty() {
        None
    } else {
        Some(Error::new(parts.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_roundtrips_message() {
        let e = Error::new("boom");
        assert_eq!(e.message(), "boom");
        assert!(e.cause().is_none());
    }

    #[test]
    fn wrap_preserves_cause_chain() {
        let inner = Error::new("io: file not found");
        let middle = Error::wrap(inner, "read config");
        let outer = Error::wrap(middle, "boot");
        assert_eq!(outer.message(), "boot");
        let chain: Vec<_> = outer.chain().map(Error::message).collect();
        assert_eq!(chain, vec!["boot", "read config", "io: file not found"]);
    }

    #[test]
    fn is_finds_any_link_in_chain() {
        let inner = Error::new("EACCES");
        let outer = Error::wrap(inner, "open /etc/passwd");
        assert!(outer.is("EACCES"));
        assert!(!outer.is("ENOENT"));
    }

    #[test]
    fn structured_fields_are_immutable_and_replace_by_key() {
        let original = Error::new("query failed");
        let tagged = original
            .with_field("sqlstate", "40001")
            .with_field("sqlstate", "23505");
        assert!(original.field("sqlstate").is_none());
        assert_eq!(tagged.field("sqlstate"), Some("23505"));
        assert_eq!(tagged.fields().len(), 1);
    }

    #[test]
    fn display_renders_chain_colon_separated() {
        let e = Error::wrap(Error::new("root"), "mid");
        assert_eq!(format!("{e}"), "mid: root");
    }

    #[test]
    fn join_empties_to_none() {
        assert!(join::<Vec<Error>>(Vec::new()).is_none());
    }

    #[test]
    fn join_semicolon_separates_matching_runtime_tiers() {
        let joined = join(vec![Error::new("a"), Error::new("b")]).unwrap();
        assert_eq!(joined.message(), "a; b");
    }
}
