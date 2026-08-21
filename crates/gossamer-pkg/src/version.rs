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

/// How a requirement compares a candidate version against the one it
/// names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionBound {
    /// `=x.y.z`, or a bare `x.y.z`: this version and no other.
    Exact,
    /// `^x.y.z`: this version or any later one.
    AtLeast,
}

/// A dependency's version requirement.
///
/// Two spellings, and no third. A bare literal pins, because a
/// manifest that names a version and gets a different one is a
/// surprise nobody asked for; `^` opts into anything newer. There is
/// deliberately no upper bound and no comparator grammar: a range with
/// a ceiling is a guess about code that has not been written yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionReq {
    /// Whether the requirement pins or sets a floor.
    pub bound: VersionBound,
    /// The version the requirement names.
    pub version: Version,
}

impl VersionReq {
    /// A requirement that accepts `version` and nothing else.
    #[must_use]
    pub fn exact(version: Version) -> Self {
        Self {
            bound: VersionBound::Exact,
            version,
        }
    }

    /// A requirement that accepts `version` or anything later.
    #[must_use]
    pub fn at_least(version: Version) -> Self {
        Self {
            bound: VersionBound::AtLeast,
            version,
        }
    }

    /// Parses `x.y.z`, `=x.y.z`, or `^x.y.z`.
    ///
    /// A leading `v` is accepted on every form, because that is how the
    /// release tags are written and a manifest that copies one should
    /// not have to remember to strip it.
    pub fn parse(text: &str) -> Result<Self, VersionError> {
        let trimmed = text.trim();
        let (bound, rest) = match trimmed.strip_prefix('^') {
            Some(rest) => (VersionBound::AtLeast, rest),
            None => (
                VersionBound::Exact,
                trimmed.strip_prefix('=').unwrap_or(trimmed),
            ),
        };
        let rest = rest.trim();
        let rest = rest.strip_prefix('v').unwrap_or(rest);
        Ok(Self {
            bound,
            version: Version::parse(rest)?,
        })
    }

    /// Whether `version` satisfies this requirement.
    ///
    /// A prerelease is selected only by a requirement that names a
    /// prerelease of the same `x.y.z`, whichever bound it carries:
    /// `^1.2.0` must not quietly resolve to `1.3.0-rc.1`, and neither
    /// must `^1.2.0-rc.1`.
    #[must_use]
    pub fn matches(&self, version: impl Borrow<Version>) -> bool {
        let version = version.borrow();
        if version.prerelease.is_some()
            && (self.version.prerelease.is_none()
                || (self.version.major, self.version.minor, self.version.patch)
                    != (version.major, version.minor, version.patch))
        {
            return false;
        }
        match self.bound {
            VersionBound::Exact => {
                // Build metadata is retained but does not participate in
                // precedence, so `Version`'s own equality decides.
                version == &self.version
            }
            VersionBound::AtLeast => version >= &self.version,
        }
    }

    /// Whether the requirement pins a single version.
    #[must_use]
    pub const fn is_exact(&self) -> bool {
        matches!(self.bound, VersionBound::Exact)
    }
}

impl fmt::Display for VersionReq {
    /// Renders the canonical spelling: a bare literal for a pin, `^`
    /// for a floor. Both re-parse to the same requirement, so a
    /// manifest this writes means what it said.
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.bound {
            VersionBound::Exact => write!(out, "{}", self.version),
            VersionBound::AtLeast => write!(out, "^{}", self.version),
        }
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
    fn a_bare_literal_pins_and_a_caret_sets_a_floor() {
        let pinned = VersionReq::parse("1.2.3").unwrap();
        assert!(pinned.is_exact());
        assert!(pinned.matches(Version::parse("1.2.3").unwrap()));
        assert!(!pinned.matches(Version::parse("1.2.4").unwrap()));
        assert!(!pinned.matches(Version::parse("1.2.2").unwrap()));

        let floor = VersionReq::parse("^1.2.3").unwrap();
        assert!(!floor.is_exact());
        assert!(floor.matches(Version::parse("1.2.3").unwrap()));
        assert!(floor.matches(Version::parse("1.2.4").unwrap()));
        assert!(floor.matches(Version::parse("2.0.0").unwrap()));
        assert!(!floor.matches(Version::parse("1.2.2").unwrap()));
    }

    #[test]
    fn an_explicit_equals_means_the_same_as_a_bare_literal() {
        assert_eq!(
            VersionReq::parse("=1.2.3").unwrap(),
            VersionReq::parse("1.2.3").unwrap()
        );
    }

    #[test]
    fn a_leading_v_is_accepted_on_every_spelling() {
        let bare = VersionReq::parse("v1.2.3").unwrap();
        assert_eq!(bare, VersionReq::exact(Version::parse("1.2.3").unwrap()));
        let floor = VersionReq::parse("^v1.2.3").unwrap();
        assert_eq!(
            floor,
            VersionReq::at_least(Version::parse("1.2.3").unwrap())
        );
    }

    #[test]
    fn every_spelling_round_trips_through_display() {
        for text in ["1.2.3", "^1.2.3"] {
            let parsed = VersionReq::parse(text).unwrap();
            assert_eq!(parsed.to_string(), text);
            assert_eq!(VersionReq::parse(&parsed.to_string()).unwrap(), parsed);
        }
    }

    #[test]
    fn a_prerelease_is_selected_only_by_a_requirement_naming_one() {
        let stable = VersionReq::parse("^1.0.0").unwrap();
        assert!(!stable.matches(Version::parse("1.1.0-beta.1").unwrap()));
        let prerelease = VersionReq::parse("^1.0.0-beta.1").unwrap();
        assert!(prerelease.matches(Version::parse("1.0.0-beta.2").unwrap()));
        // A prerelease floor does not drag in a later base tuple's
        // prereleases, which is the surprise the confinement prevents.
        assert!(!prerelease.matches(Version::parse("1.1.0-beta.1").unwrap()));
    }

    #[test]
    fn rejects_invalid_semver_identifiers() {
        for invalid in ["01.0.0", "1.0.0-01", "1.0.0-", "1.0.0+", "1.0"] {
            assert!(Version::parse(invalid).is_err(), "{invalid}");
        }
    }
}
