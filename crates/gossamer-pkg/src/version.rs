//! Semver `MAJOR.MINOR.PATCH` plus the `^x.y.z` range form used by
//! the manifest resolver (SPEC §16.4).

#![forbid(unsafe_code)]

use std::cmp::Ordering;
use std::fmt;

use thiserror::Error;

/// Strict `MAJOR.MINOR.PATCH` version. Pre-release / build metadata
/// suffixes are accepted lexically but currently discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Version {
    /// Major component.
    pub major: u32,
    /// Minor component.
    pub minor: u32,
    /// Patch component.
    pub patch: u32,
}

impl Version {
    /// Constructs a version directly from its components.
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parses a semver string. Pre-release / build metadata after the
    /// patch are stripped.
    pub fn parse(text: &str) -> Result<Self, VersionError> {
        let core = text.split(['+', '-']).next().unwrap_or(text);
        let mut parts = core.split('.');
        let major = parse_segment(parts.next(), text)?;
        let minor = parse_segment(parts.next(), text)?;
        let patch = parse_segment(parts.next(), text)?;
        if parts.next().is_some() {
            return Err(VersionError::Malformed(text.to_string()));
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
    }
}

fn parse_segment(part: Option<&str>, full: &str) -> Result<u32, VersionError> {
    let segment = part.ok_or_else(|| VersionError::Malformed(full.to_string()))?;
    segment
        .parse::<u32>()
        .map_err(|_| VersionError::Malformed(full.to_string()))
}

/// Errors raised by [`Version::parse`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VersionError {
    /// The input was not a valid `MAJOR.MINOR.PATCH` triple.
    #[error("malformed version {0:?}")]
    Malformed(String),
}

/// Caret range `^x.y.z` per SPEC §16.4. Matches everything from the
/// minimum up to (exclusive) the next major boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaretRange {
    /// Inclusive minimum version.
    pub minimum: Version,
}

impl CaretRange {
    /// Constructs a caret range with `minimum` as the lower bound.
    #[must_use]
    pub const fn new(minimum: Version) -> Self {
        Self { minimum }
    }

    /// Parses a `^x.y.z` or `x.y.z` literal. The leading `^` is
    /// optional because the manifest format treats a bare version
    /// literal as a caret range (SPEC §16.4 default).
    pub fn parse(text: &str) -> Result<Self, VersionError> {
        let stripped = text.trim().strip_prefix('^').unwrap_or(text.trim());
        let minimum = Version::parse(stripped)?;
        Ok(Self { minimum })
    }

    /// Returns whether `version` is satisfied by this range.
    #[must_use]
    pub fn matches(&self, version: Version) -> bool {
        if version < self.minimum {
            return false;
        }
        // For 0.x.y, a caret range pins to the same minor; for x.y.z
        // (x ≥ 1) it pins to the same major.
        if self.minimum.major == 0 {
            self.minimum.major == version.major && self.minimum.minor == version.minor
        } else {
            self.minimum.major == version.major
        }
    }
}

impl fmt::Display for CaretRange {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "^{}", self.minimum)
    }
}
