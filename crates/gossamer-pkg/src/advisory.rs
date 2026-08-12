//! Security advisories: the feed format, its signature, and matching it
//! against a resolved dependency set.
//!
//! An advisory feed is another artefact through the machinery that
//! already carries the registry index: fetched over HTTPS, verified
//! against the same Ed25519 trust root a project pins in
//! `[trusted-publishers]`, and cached by content.
//!
//! The property worth having is precision. An advisory naming an item
//! nothing in the project reaches is noise, and noise is what makes a
//! security tool get switched off. The affected item paths are part of
//! the format for exactly that reason.

use std::collections::BTreeSet;

use crate::version::Version;

/// One advisory as published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advisory {
    /// Stable identifier, e.g. `GOSA-2026-0001`.
    pub id: String,
    /// Package the advisory is about.
    pub package: String,
    /// Versions affected, inclusive lower bound.
    pub affected_from: Version,
    /// First version that is not affected. `None` means every version
    /// from `affected_from` onward.
    pub fixed_in: Option<Version>,
    /// Item paths whose use exposes the flaw. An empty list means the
    /// whole package is affected however it is used.
    pub affected_items: Vec<String>,
    /// Severity as published: `low` / `medium` / `high` / `critical`.
    pub severity: String,
    /// One-line description.
    pub summary: String,
}

impl Advisory {
    /// Whether `version` of this advisory's package is affected.
    #[must_use]
    pub fn affects_version(&self, version: &Version) -> bool {
        if *version < self.affected_from {
            return false;
        }
        match &self.fixed_in {
            Some(fixed) => version < fixed,
            None => true,
        }
    }

    /// Whether the advisory can reach a project that references
    /// `referenced`.
    ///
    /// An advisory with no item list affects the package however it is
    /// used and is always reachable. Otherwise it is reachable only when
    /// the project references one of the named items - the property that
    /// keeps the report short enough to act on.
    #[must_use]
    pub fn is_reachable(&self, referenced: &BTreeSet<String>) -> bool {
        self.affected_items.is_empty()
            || self
                .affected_items
                .iter()
                .any(|item| referenced.contains(item))
    }
}

/// Parses the advisory feed: a JSON array of advisory objects.
///
/// Deliberately tolerant of unknown keys so a feed can gain fields
/// without every older toolchain refusing to read it.
///
/// # Errors
///
/// Returns a message naming the first malformed entry.
pub fn parse_feed(source: &str) -> Result<Vec<Advisory>, String> {
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(source).map_err(|e| format!("advisory feed: {e}"))?;
    let mut out = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let field = |name: &str| {
            entry
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        let id = field("id").ok_or_else(|| format!("advisory {index}: missing `id`"))?;
        let package =
            field("package").ok_or_else(|| format!("advisory {id}: missing `package`"))?;
        let affected_from = field("affected_from")
            .ok_or_else(|| format!("advisory {id}: missing `affected_from`"))?;
        let affected_from = Version::parse(&affected_from)
            .map_err(|e| format!("advisory {id}: affected_from: {e}"))?;
        let fixed_in = match field("fixed_in") {
            Some(text) => {
                Some(Version::parse(&text).map_err(|e| format!("advisory {id}: fixed_in: {e}"))?)
            }
            None => None,
        };
        let affected_items = entry
            .get("affected_items")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|i| i.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        out.push(Advisory {
            id: id.clone(),
            package,
            affected_from,
            fixed_in,
            affected_items,
            severity: field("severity").unwrap_or_else(|| "unknown".to_string()),
            summary: field("summary").unwrap_or_default(),
        });
    }
    Ok(out)
}

/// Path of the advisory feed relative to a registry root.
pub const FEED_PATH: &str = "advisories/index.json";

/// Path of the feed's detached Ed25519 signature.
pub const FEED_SIGNATURE_PATH: &str = "advisories/index.json.sig";

/// Fetches the advisory feed from `registry_url` and verifies its
/// signature against `trusted_key` before parsing.
///
/// The trust root is the project's, not the registry's: an advisory feed
/// that could be rewritten by whoever serves it can hide an advisory as
/// easily as invent one. A project that pins no key gets no remote feed
/// rather than an unverified one - refusing is the safe direction, since
/// a silently unverified feed reads exactly like a verified one.
///
/// # Errors
///
/// Returns a message when the fetch fails, the signature does not
/// verify, or the feed does not parse.
pub fn fetch_verified_feed(
    transport: &dyn crate::transport::Transport,
    registry_url: &str,
    trusted_key: &crate::signing::VerifyingKey,
) -> Result<Vec<Advisory>, String> {
    let base = registry_url.trim_end_matches('/');
    let feed = transport
        .get(&format!("{base}/{FEED_PATH}"))
        .map_err(|e| format!("fetching the advisory feed: {e}"))?;
    let signature = transport
        .get(&format!("{base}/{FEED_SIGNATURE_PATH}"))
        .map_err(|e| format!("fetching the advisory feed signature: {e}"))?;
    trusted_key
        .verify(&feed, &signature)
        .map_err(|e| format!("the advisory feed's signature does not verify: {e}"))?;
    let text = String::from_utf8(feed).map_err(|e| format!("advisory feed is not UTF-8: {e}"))?;
    parse_feed(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advisory(from: &str, fixed: Option<&str>, items: &[&str]) -> Advisory {
        Advisory {
            id: "GOSA-2026-0001".into(),
            package: "example.com/lib".into(),
            affected_from: Version::parse(from).unwrap(),
            fixed_in: fixed.map(|f| Version::parse(f).unwrap()),
            affected_items: items.iter().map(|i| (*i).to_string()).collect(),
            severity: "high".into(),
            summary: "example".into(),
        }
    }

    #[test]
    fn a_version_below_the_range_is_unaffected() {
        let a = advisory("1.2.0", Some("1.3.0"), &[]);
        assert!(!a.affects_version(&Version::parse("1.1.9").unwrap()));
        assert!(a.affects_version(&Version::parse("1.2.0").unwrap()));
        assert!(a.affects_version(&Version::parse("1.2.9").unwrap()));
        assert!(!a.affects_version(&Version::parse("1.3.0").unwrap()));
    }

    #[test]
    fn an_open_ended_advisory_affects_every_later_version() {
        let a = advisory("1.2.0", None, &[]);
        assert!(a.affects_version(&Version::parse("9.9.9").unwrap()));
    }

    #[test]
    fn an_advisory_naming_items_is_reachable_only_through_them() {
        let a = advisory("1.0.0", None, &["lib::parse", "lib::decode"]);
        let mut referenced = BTreeSet::new();
        referenced.insert("lib::encode".to_string());
        assert!(
            !a.is_reachable(&referenced),
            "an advisory nothing reaches is noise"
        );
        referenced.insert("lib::decode".to_string());
        assert!(a.is_reachable(&referenced));
    }

    #[test]
    fn an_advisory_with_no_items_affects_any_use() {
        let a = advisory("1.0.0", None, &[]);
        assert!(a.is_reachable(&BTreeSet::new()));
    }

    /// The feed is verified against the project's own trust root, so a
    /// registry that rewrites it cannot hide an advisory or invent one.
    #[test]
    fn a_tampered_feed_is_refused() {
        use crate::signing::SigningKey;
        use crate::transport::Transport;

        struct Canned {
            feed: Vec<u8>,
            signature: Vec<u8>,
        }
        impl Transport for Canned {
            fn get(&self, url: &str) -> Result<Vec<u8>, crate::transport::TransportError> {
                if std::path::Path::new(url)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("sig"))
                {
                    Ok(self.signature.clone())
                } else {
                    Ok(self.feed.clone())
                }
            }
        }

        let feed = br#"[{"id":"GOSA-1","package":"example.com/lib","affected_from":"1.0.0"}]"#;
        let key = SigningKey::from_bytes([7u8; 32]);
        let signature = key.sign(feed).to_vec();
        let verifying = key.verifying_key();

        let honest = Canned {
            feed: feed.to_vec(),
            signature: signature.clone(),
        };
        let parsed = fetch_verified_feed(&honest, "https://example.invalid", &verifying)
            .expect("an honestly signed feed parses");
        assert_eq!(parsed.len(), 1);

        let mut tampered_bytes = feed.to_vec();
        tampered_bytes[3] = b'X';
        let tampered = Canned {
            feed: tampered_bytes,
            signature,
        };
        let err = fetch_verified_feed(&tampered, "https://example.invalid", &verifying)
            .expect_err("a rewritten feed must be refused");
        assert!(err.contains("does not verify"), "{err}");
    }

    #[test]
    fn a_feed_parses_and_reports_the_first_bad_entry() {
        let feed = r#"[
          {"id":"GOSA-1","package":"example.com/lib","affected_from":"1.0.0",
           "fixed_in":"1.0.1","affected_items":["lib::parse"],
           "severity":"high","summary":"bad parse"}
        ]"#;
        let parsed = parse_feed(feed).expect("feed parses");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].affected_items, vec!["lib::parse".to_string()]);

        let bad = r#"[{"package":"example.com/lib","affected_from":"1.0.0"}]"#;
        assert!(parse_feed(bad).unwrap_err().contains("missing `id`"));
    }
}
