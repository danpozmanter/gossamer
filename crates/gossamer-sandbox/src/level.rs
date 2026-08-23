//! Security levels and the host capability report.
//!
//! A level name means the same guarantee on every operating system. A
//! host that cannot meet a level reports it unavailable; it never
//! offers a weaker thing under the same name, which is what makes the
//! other levels believable.

use serde::{Deserialize, Serialize};

/// How much of the machine a sandbox holds back.
///
/// Ordered, so "at least this strong" is a comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// No sandbox. The child runs exactly as the caller would.
    #[default]
    None,
    /// Environment allowlist, private temp directory, descriptor and
    /// handle hygiene, and process-tree cleanup. No kernel
    /// enforcement, and available everywhere.
    Basic,
    /// `basic` plus an OS-enforced filesystem policy and network
    /// denial, inherited by every descendant.
    Standard,
    /// `standard` plus process-table isolation and a reduced kernel
    /// surface.
    Strict,
}

impl Level {
    /// The level named by `text`, or `None` when it names none.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "none" => Some(Self::None),
            "basic" => Some(Self::Basic),
            "standard" => Some(Self::Standard),
            "strict" => Some(Self::Strict),
            _ => None,
        }
    }

    /// The spelling the command line and manifests accept.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Basic => "basic",
            Self::Standard => "standard",
            Self::Strict => "strict",
        }
    }
}

impl std::fmt::Display for Level {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(self.as_str())
    }
}

/// How completely one dimension of a policy is enforced on this host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind", content = "reason")]
pub enum Enforcement {
    /// The policy is enforced as written.
    Full,
    /// Part of the policy is enforced; the reason names what is not.
    Partial(String),
    /// Nothing in this dimension is enforced.
    None,
}

impl Enforcement {
    /// Whether anything at all is enforced.
    #[must_use]
    pub const fn is_enforced(&self) -> bool {
        !matches!(self, Self::None)
    }
}

impl std::fmt::Display for Enforcement {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => out.write_str("full"),
            Self::Partial(reason) => write!(out, "partial ({reason})"),
            Self::None => out.write_str("none"),
        }
    }
}

/// Which backend is answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    /// Landlock, namespaces, seccomp, cgroup v2.
    Linux,
    /// Seatbelt profiles.
    MacOs,
    /// Restricted tokens, job objects, `AppContainer`.
    Windows,
    /// A target with no backend.
    Unsupported,
}

impl std::fmt::Display for Platform {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str(match self {
            Self::Linux => "linux",
            Self::MacOs => "macos",
            Self::Windows => "windows",
            Self::Unsupported => "unsupported",
        })
    }
}

/// What this host can actually honor.
///
/// Produced by probing, never by assuming: every field answers for the
/// machine the report was taken on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxCapabilities {
    /// Backend answering for this host.
    pub platform: Platform,
    /// Human-readable OS description, e.g. `Linux 7.0.0-28-generic x86_64`.
    pub os_description: String,
    /// Filesystem-policy enforcement.
    pub filesystem: Enforcement,
    /// Network-denial enforcement.
    pub network: Enforcement,
    /// Process-table and signal isolation.
    pub process_isolation: Enforcement,
    /// Highest level this host can honor.
    pub max_level: Level,
    /// Everything a caller has to know that the fields above cannot
    /// say, such as which Landlock ABI the kernel reports or which
    /// sysctl blocks `strict`.
    pub notes: Vec<String>,
}

impl SandboxCapabilities {
    /// A report for a host that enforces nothing.
    #[must_use]
    pub fn unsupported(platform: Platform, os_description: String, note: &str) -> Self {
        Self {
            platform,
            os_description,
            filesystem: Enforcement::None,
            network: Enforcement::None,
            process_isolation: Enforcement::None,
            max_level: Level::None,
            notes: vec![note.to_string()],
        }
    }

    /// The report as a JSON document, for `doctor --json` and for test
    /// oracles that would otherwise re-encode it by hand.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(test)]
mod level_tests {
    use super::*;

    #[test]
    fn levels_order_by_increasing_restriction() {
        assert!(Level::None < Level::Basic);
        assert!(Level::Basic < Level::Standard);
        assert!(Level::Standard < Level::Strict);
    }

    #[test]
    fn level_spellings_round_trip() {
        for level in [Level::None, Level::Basic, Level::Standard, Level::Strict] {
            assert_eq!(Level::parse(level.as_str()), Some(level));
        }
        assert_eq!(Level::parse("paranoid"), None);
    }

    #[test]
    fn a_capability_report_serializes_to_json() {
        let report = SandboxCapabilities::unsupported(
            Platform::Unsupported,
            "test".to_string(),
            "no backend",
        );
        let json = report.to_json();
        assert!(json.contains("\"max_level\": \"none\""), "{json}");
        assert!(json.contains("no backend"), "{json}");
    }
}
