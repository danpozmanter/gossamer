//! Project identifiers per SPEC §6.5.

#![forbid(unsafe_code)]

use std::fmt;

use thiserror::Error;

/// Validated project identifier of the form
/// `domain.tld[/path/segments]` (e.g. `example.com/math`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectId {
    raw: String,
    domain_end: usize,
}

impl ProjectId {
    /// Parses and validates a project identifier.
    pub fn parse(raw: &str) -> Result<Self, ProjectIdError> {
        if raw.is_empty() {
            return Err(ProjectIdError::Empty);
        }
        let domain_end = raw.find('/').unwrap_or(raw.len());
        let domain = &raw[..domain_end];
        if !is_valid_domain(domain) {
            return Err(ProjectIdError::InvalidDomain(domain.to_string()));
        }
        if domain_end < raw.len() {
            for segment in raw[domain_end + 1..].split('/') {
                if !is_valid_path_segment(segment) {
                    return Err(ProjectIdError::InvalidSegment(segment.to_string()));
                }
            }
        }
        Ok(Self {
            raw: raw.to_string(),
            domain_end,
        })
    }

    /// Returns the canonical text form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Returns the DNS-prefix portion (everything before the first
    /// `/`).
    #[must_use]
    pub fn domain(&self) -> &str {
        &self.raw[..self.domain_end]
    }

    /// Returns the path-segment portion (everything after the first
    /// `/`), or an empty string if absent.
    #[must_use]
    pub fn path(&self) -> &str {
        if self.domain_end < self.raw.len() {
            &self.raw[self.domain_end + 1..]
        } else {
            ""
        }
    }

    /// Returns the last `/`-separated path component, falling back
    /// to the domain when there are no path segments. The compiler
    /// uses this as the default `use` binding name.
    #[must_use]
    pub fn tail(&self) -> &str {
        let path = self.path();
        // `"".rsplit('/')` yields one empty item rather than none, so the
        // no-path case has to be answered before the split rather than by a
        // fallback after it.
        if path.is_empty() {
            return self.domain();
        }
        path.rsplit('/').next().unwrap_or(path)
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(&self.raw)
    }
}

/// Errors raised by [`ProjectId::parse`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProjectIdError {
    /// Empty input.
    #[error("project identifier is empty")]
    Empty,
    /// The DNS-prefix portion was malformed.
    #[error(
        "invalid domain segment {0:?}: project ids start with a dotted lowercase domain \
         (e.g. \"example.com/app\"); bare single-segment ids are reserved for the standard library"
    )]
    InvalidDomain(String),
    /// A path segment after the domain was malformed.
    #[error("invalid path segment {0:?}")]
    InvalidSegment(String),
}

fn is_valid_domain(domain: &str) -> bool {
    if !domain.contains('.') {
        return false;
    }
    domain.split('.').all(is_valid_label)
}

fn is_valid_label(label: &str) -> bool {
    let mut chars = label.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_valid_path_segment(segment: &str) -> bool {
    let mut chars = segment.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::{ProjectId, ProjectIdError};

    #[test]
    fn parses_simple_identifier() {
        let id = ProjectId::parse("example.com/math").unwrap();
        assert_eq!(id.domain(), "example.com");
        assert_eq!(id.path(), "math");
        assert_eq!(id.tail(), "math");
    }

    #[test]
    fn a_domain_only_id_tails_to_its_domain() {
        // A reverse-DNS id with no path segment is what names the binary a
        // `gos build` writes, so an empty tail leaves the output path a bare
        // directory and the link fails.
        let id = ProjectId::parse("net.example.postwisp").unwrap();
        assert_eq!(id.path(), "");
        assert_eq!(id.tail(), "net.example.postwisp");
    }

    #[test]
    fn path_segments_accept_underscores() {
        let id = ProjectId::parse("example.com/my_project").unwrap();
        assert_eq!(id.path(), "my_project");

        let nested = ProjectId::parse("acme.dev/sub_dir/leaf_name").unwrap();
        assert_eq!(nested.tail(), "leaf_name");
    }

    #[test]
    fn path_segments_still_accept_hyphens() {
        let id = ProjectId::parse("example.com/my-project").unwrap();
        assert_eq!(id.path(), "my-project");
    }

    #[test]
    fn path_segments_reject_leading_underscore() {
        // Lead char is still `[a-z0-9]`; underscores are only allowed
        // mid-segment. Otherwise package names could look like Rust's
        // unused-binding convention (`_foo`) which is reserved.
        let err = ProjectId::parse("example.com/_lead").unwrap_err();
        assert!(matches!(err, ProjectIdError::InvalidSegment(_)));
    }

    #[test]
    fn domains_still_reject_underscores() {
        // DNS labels disallow `_`; keep that for the domain even though
        // path segments accept it.
        let err = ProjectId::parse("bad_host.example/path").unwrap_err();
        assert!(matches!(err, ProjectIdError::InvalidDomain(_)));
    }
}
