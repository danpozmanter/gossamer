//! The toolchain version a project states it is written against.

use crate::Version;

/// This toolchain's own version, as `gos --version` reports it.
#[must_use]
pub fn toolchain_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Version {
        major: 0,
        minor: 0,
        patch: 0,
        prerelease: None,
        build: None,
    })
}

/// Parses the `gossamer-version` manifest spelling: an exact toolchain
/// version, with or without the `v` the release tag carries.
///
/// # Errors
/// Returns the source text when it is not a version.
pub fn parse_gossamer_version(value: &str) -> Result<Version, String> {
    let trimmed = value.strip_prefix('v').unwrap_or(value);
    Version::parse(trimmed).map_err(|_| value.to_string())
}
