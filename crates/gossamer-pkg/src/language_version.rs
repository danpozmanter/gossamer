//! The toolchain version a project states it is written against.

use crate::{Version, VersionReq};

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

/// Parses the `gossamer-version` manifest spelling.
///
/// A bare version (or an explicit `=`) names one toolchain and no
/// other; `^` names it as a floor. The `v` the release tag carries is
/// accepted on every form.
///
/// # Errors
/// Returns the source text when it is not a version requirement.
pub fn parse_gossamer_version(value: &str) -> Result<VersionReq, String> {
    VersionReq::parse(value).map_err(|_| value.to_string())
}
