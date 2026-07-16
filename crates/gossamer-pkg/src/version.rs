//! `SemVer` `MAJOR.MINOR.PATCH[-PRERELEASE][+BUILD]` plus the `^x.y.z`
//! range form used by the manifest resolver (SPEC §16.4).

#![forbid(unsafe_code)]

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::fmt;

use thiserror::Error;

/// Strict Semantic Versioning 2.0.0 version.
///
/// Build metadata is retained for display and lockfile fidelity but does not
/// affect precedence. Pre-release identifiers participate in precedence, so a
/// registry cannot accidentally resolve `1.0.0-alpha` as the final `1.0.0`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    /// Major component.
    pub major: u32,
    /// Minor component.
    pub minor: u32,
    /// Patch component.
    pub patch: u32,
    /// Dot-separated prerelease identifiers, without the leading `-`.
    pub prerelease: Option<String>,
    /// Dot-separated build metadata, without the leading `+`.
    pub build: Option<String>,
}

impl Version {
    /// Constructs a version directly from its components.
    #[must_use]
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            prerelease: None,
            build: None,
        }
    }

    /// Parses a strict Semantic Versioning 2.0.0 string.
    pub fn parse(text: &str) -> Result<Self, VersionError> {
        let (without_build, build) = match text.split_once('+') {
            Some((core, build)) => (core, Some(build)),
            None => (text, None),
        };
        if build.is_some_and(|value| !valid_identifiers(value, false)) {
            return Err(VersionError::Malformed(text.to_string()));
        }
        let (core, prerelease) = match without_build.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (without_build, None),
        };
        if prerelease.is_some_and(|value| !valid_identifiers(value, true)) {
            return Err(VersionError::Malformed(text.to_string()));
        }
        let mut parts = core.split('.');
        let major = parse_numeric_segment(parts.next(), text)?;
        let minor = parse_numeric_segment(parts.next(), text)?;
        let patch = parse_numeric_segment(parts.next(), text)?;
        if parts.next().is_some() {
            return Err(VersionError::Malformed(text.to_string()));
        }
        Ok(Self {
            major,
            minor,
            patch,
            prerelease: prerelease.map(ToOwned::to_owned),
            build: build.map(ToOwned::to_owned),
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(prerelease) = &self.prerelease {
            write!(out, "-{prerelease}")?;
        }
        if let Some(build) = &self.build {
            write!(out, "+{build}")?;
        }
        Ok(())
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
            .then_with(|| cmp_prerelease(self.prerelease.as_deref(), other.prerelease.as_deref()))
            // SemVer build metadata has no precedence. The deterministic tie
            // break keeps `Ord` consistent with `Eq` for catalogue storage.
            .then(self.build.cmp(&other.build))
    }
}

fn parse_numeric_segment(part: Option<&str>, full: &str) -> Result<u32, VersionError> {
    let segment = part.ok_or_else(|| VersionError::Malformed(full.to_string()))?;
    if segment.is_empty() || (segment.len() > 1 && segment.starts_with('0')) {
        return Err(VersionError::Malformed(full.to_string()));
    }
    segment
        .parse::<u32>()
        .map_err(|_| VersionError::Malformed(full.to_string()))
}

fn valid_identifiers(value: &str, reject_zero_padded_numeric: bool) -> bool {
    !value.is_empty()
        && value.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && (!reject_zero_padded_numeric
                    || !identifier.bytes().all(|byte| byte.is_ascii_digit())
                    || identifier.len() == 1
                    || !identifier.starts_with('0'))
        })
}

fn cmp_prerelease(left: Option<&str>, right: Option<&str>) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => {
            let mut left = left.split('.');
            let mut right = right.split('.');
            loop {
                match (left.next(), right.next()) {
                    (None, None) => return Ordering::Equal,
                    (None, Some(_)) => return Ordering::Less,
                    (Some(_), None) => return Ordering::Greater,
                    (Some(left), Some(right)) => {
                        let left_numeric = left.bytes().all(|byte| byte.is_ascii_digit());
                        let right_numeric = right.bytes().all(|byte| byte.is_ascii_digit());
                        let order = match (left_numeric, right_numeric) {
                            (true, true) => left
                                .parse::<u64>()
                                .expect("validated prerelease number")
                                .cmp(&right.parse::<u64>().expect("validated prerelease number")),
                            (true, false) => Ordering::Less,
                            (false, true) => Ordering::Greater,
                            (false, false) => left.cmp(right),
                        };
                        if order != Ordering::Equal {
                            return order;
                        }
                    }
                }
            }
        }
    }
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaretRange {
    /// Inclusive minimum version.
    pub minimum: Version,
}

impl CaretRange {
    /// Constructs a caret range with `minimum` as the lower bound.
    #[must_use]
    pub fn new(minimum: Version) -> Self {
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
    pub fn matches(&self, version: impl Borrow<Version>) -> bool {
        let version = version.borrow();
        // A normal caret requirement does not opt into prereleases. A
        // prerelease minimum explicitly does, but only within its base tuple.
        if version.prerelease.is_some()
            && (self.minimum.prerelease.is_none()
                || (self.minimum.major, self.minimum.minor, self.minimum.patch)
                    != (version.major, version.minor, version.patch))
        {
            return false;
        }
        if version < &self.minimum {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_orders_prereleases_by_semver_precedence() {
        let ordered = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        let parsed: Vec<Version> = ordered
            .iter()
            .map(|value| Version::parse(value).unwrap())
            .collect();
        assert!(parsed.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn caret_ranges_exclude_prereleases_unless_explicitly_requested() {
        let stable = CaretRange::parse("^1.0.0").unwrap();
        assert!(!stable.matches(Version::parse("1.1.0-beta.1").unwrap()));
        let prerelease = CaretRange::parse("^1.0.0-beta.1").unwrap();
        assert!(prerelease.matches(Version::parse("1.0.0-beta.2").unwrap()));
        assert!(!prerelease.matches(Version::parse("1.1.0-beta.1").unwrap()));
    }

    #[test]
    fn rejects_invalid_semver_identifiers() {
        for invalid in ["01.0.0", "1.0.0-01", "1.0.0-", "1.0.0+", "1.0"] {
            assert!(Version::parse(invalid).is_err(), "{invalid}");
        }
    }
}
