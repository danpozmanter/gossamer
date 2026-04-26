//! Real-tarball fetch path through the pluggable transport.
//!
//! Validates the single useful thing "part 1" of the
//! `the risks backlog` package-registry item was scoped to deliver:
//! a fetcher that consumes a manifest-declared URL, checks the
//! downloaded bytes against a pinned SHA-256, and unpacks the
//! resulting tarball into the cache. The `StaticTransport` test
//! double stands in for a real HTTP server so this test runs
//! offline in CI.

use std::collections::BTreeMap;
use std::sync::Arc;

use gossamer_pkg::{
    Cache, CacheError, FetchOptions, Fetcher, ProjectId, Resolved, ResolvedSource, StaticTransport,
    Transport, sha256,
};

/// Builds a single-entry USTAR tarball in memory so tests do not
/// need `tar(1)` on the host. Keeps the helper here — not in
/// `gossamer-pkg` — because it's purely test-support code.
fn build_tar(name: &str, body: &[u8]) -> Vec<u8> {
    let mut header = [0u8; 512];
    for (i, b) in name.as_bytes().iter().take(100).enumerate() {
        header[i] = *b;
    }
    for (i, b) in b"0000644\0".iter().enumerate() {
        header[100 + i] = *b;
    }
    let size_octal = format!("{:011o}\0", body.len());
    for (i, b) in size_octal.as_bytes().iter().take(12).enumerate() {
        header[124 + i] = *b;
    }
    for (i, b) in b"00000000000\0".iter().enumerate() {
        header[136 + i] = *b;
    }
    for cell in &mut header[148..156] {
        *cell = b' ';
    }
    header[156] = b'0';
    for (i, b) in b"ustar\0".iter().enumerate() {
        header[257 + i] = *b;
    }
    header[263] = b'0';
    header[264] = b'0';
    let checksum: u32 = header.iter().map(|b| u32::from(*b)).sum();
    let cs = format!("{checksum:06o}\0 ");
    for (i, b) in cs.as_bytes().iter().take(8).enumerate() {
        header[148 + i] = *b;
    }
    let mut out = Vec::with_capacity(1024);
    out.extend_from_slice(&header);
    out.extend_from_slice(body);
    let pad = (512 - body.len() % 512) % 512;
    out.resize(out.len() + pad, 0);
    out.extend_from_slice(&[0u8; 1024]);
    out
}

fn resolved_for(url: &str, hash: &str) -> Resolved {
    Resolved {
        id: ProjectId::parse("example.com/demo").unwrap(),
        pin: ResolvedSource::Tarball {
            url: url.to_string(),
            sha256: hash.to_string(),
        },
    }
}

#[test]
fn tarball_fetch_verifies_sha256_and_unpacks_into_the_cache() {
    let tar_bytes = build_tar("src/main.gos", b"fn main() { }\n");
    let expected = sha256::hex(&tar_bytes);
    let url = "https://example.com/demo-0.1.0.tar";
    let mut transport = StaticTransport::new();
    transport.insert(url, tar_bytes.clone());
    let fetcher = Fetcher::with_transport(
        FetchOptions::default(),
        Arc::new(transport) as Arc<dyn Transport>,
    );
    let mut cache = Cache::new();
    let resolved = resolved_for(url, &expected);
    let outcome = fetcher
        .fetch_all(&[resolved], &mut cache)
        .expect("fetch should succeed with matching sha256");
    assert_eq!(outcome.len(), 1);
    let files: &BTreeMap<String, Vec<u8>> = &outcome[0].source.files;
    assert_eq!(
        files.get("src/main.gos").map(Vec::as_slice),
        Some(b"fn main() { }\n" as &[u8])
    );
}

#[test]
fn tarball_fetch_rejects_payload_that_fails_sha256_verification() {
    let tar_bytes = build_tar("src/main.gos", b"honest\n");
    // Lie about the expected digest: flip the first hex char.
    let valid_digest = sha256::hex(&tar_bytes);
    let mut tampered = valid_digest.clone();
    tampered.replace_range(..1, "0");
    if tampered == valid_digest {
        tampered.replace_range(..1, "f");
    }
    let url = "https://example.com/demo-0.1.0.tar";
    let mut transport = StaticTransport::new();
    transport.insert(url, tar_bytes);
    let fetcher = Fetcher::with_transport(
        FetchOptions::default(),
        Arc::new(transport) as Arc<dyn Transport>,
    );
    let mut cache = Cache::new();
    let resolved = resolved_for(url, &tampered);
    let err = fetcher
        .fetch_all(&[resolved], &mut cache)
        .expect_err("fetch must reject a payload whose sha256 does not match the pin");
    assert!(
        matches!(err, CacheError::DigestMismatch { .. }),
        "expected DigestMismatch, got {err:?}"
    );
}

#[test]
fn tarball_fetch_bubbles_transport_errors_cleanly() {
    // Empty transport: any URL lookup fails. The cache must stay
    // untouched, and the error must be a clean `Unsupported` wrap so
    // the CLI has a message to render.
    let fetcher = Fetcher::with_transport(
        FetchOptions::default(),
        Arc::new(StaticTransport::new()) as Arc<dyn Transport>,
    );
    let mut cache = Cache::new();
    let resolved = resolved_for("https://example.com/missing.tar", "0".repeat(64).as_str());
    let err = fetcher
        .fetch_all(&[resolved], &mut cache)
        .expect_err("fetch must surface a transport error");
    assert!(
        matches!(err, CacheError::Unsupported(_)),
        "expected Unsupported wrap of TransportError, got {err:?}"
    );
}
