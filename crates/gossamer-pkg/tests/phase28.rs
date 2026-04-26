//!: cache, fetcher, vendor.

use std::collections::BTreeMap;

use gossamer_pkg::{
    Cache, CacheError, CachedSource, FetchOptions, Fetcher, ProjectId, Resolved, ResolvedSource,
    Version, vendor,
};

fn synth_resolved(id: &str, pin: ResolvedSource) -> Resolved {
    Resolved {
        id: ProjectId::parse(id).unwrap(),
        pin,
    }
}

#[test]
fn sha256_hex_matches_known_vector() {
    use gossamer_pkg::sha256;
    assert_eq!(
        sha256::hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256::hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn cached_source_is_deterministic_across_builds() {
    let mut a = BTreeMap::new();
    a.insert("src/main.gos".to_string(), b"fn main() {}\n".to_vec());
    a.insert("README.md".to_string(), b"# hello\n".to_vec());
    let id = ProjectId::parse("example.com/widget").unwrap();
    let src_a = CachedSource::build(id.clone(), a.clone());
    let src_b = CachedSource::build(id, a);
    assert_eq!(src_a.digest, src_b.digest);
    assert_eq!(src_a.digest.len(), 64);
}

#[test]
fn fetcher_caches_path_sources_from_disk() {
    let mut tmp = std::env::temp_dir();
    tmp.push(format!("gossamer-fetch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("project.toml"), b"# stub\n").unwrap();
    std::fs::write(tmp.join("src/main.gos"), b"fn main() {}\n").unwrap();
    let resolved = synth_resolved(
        "example.com/local",
        ResolvedSource::Path(tmp.to_string_lossy().into_owned()),
    );
    let mut cache = Cache::new();
    let fetched = Fetcher::default()
        .fetch_all(&[resolved], &mut cache)
        .unwrap();
    assert_eq!(fetched.len(), 1);
    assert!(fetched[0].source.files.contains_key("src/main.gos"));
    assert!(cache.contains(&fetched[0].source.digest));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn fetcher_records_synthetic_payloads_for_git_and_registry() {
    let entries = vec![
        synth_resolved(
            "example.com/git",
            ResolvedSource::Git {
                url: "https://git/x.git".to_string(),
                reference: "v1.0.0".to_string(),
            },
        ),
        synth_resolved(
            "example.com/reg",
            ResolvedSource::Registry(Version::new(1, 2, 3)),
        ),
    ];
    let mut cache = Cache::new();
    let fetched = Fetcher::default().fetch_all(&entries, &mut cache).unwrap();
    assert_eq!(fetched.len(), 2);
    assert_ne!(fetched[0].source.digest, fetched[1].source.digest);
    assert_eq!(cache.len(), 2);
}

#[test]
fn offline_mode_refuses_unseen_entries() {
    let resolved = synth_resolved(
        "example.com/git",
        ResolvedSource::Git {
            url: "https://git/x.git".to_string(),
            reference: "main".to_string(),
        },
    );
    let mut cache = Cache::new();
    let result = Fetcher::new(FetchOptions { offline: true }).fetch_all(&[resolved], &mut cache);
    assert!(matches!(result, Err(CacheError::Unsupported(_))));
}

#[test]
fn tarball_without_transport_entry_reports_transport_error() {
    let resolved = synth_resolved(
        "example.com/tar",
        ResolvedSource::Tarball {
            url: "https://example.com/x.tgz".to_string(),
            sha256: "0".repeat(64),
        },
    );
    let mut cache = Cache::new();
    let err = Fetcher::default()
        .fetch_all(&[resolved], &mut cache)
        .unwrap_err();
    assert!(
        matches!(err, CacheError::Unsupported(_)),
        "default transport is empty; a Tarball must surface the transport error. got: {err:?}"
    );
}

#[test]
fn vendor_writes_per_project_subdirs() {
    let entries = vec![synth_resolved(
        "example.com/widget",
        ResolvedSource::Registry(Version::new(0, 1, 0)),
    )];
    let mut cache = Cache::new();
    let fetched = Fetcher::default().fetch_all(&entries, &mut cache).unwrap();

    let mut dest = std::env::temp_dir();
    dest.push(format!("gossamer-vendor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);
    let written = vendor(&fetched, &dest).unwrap();
    assert_eq!(written.len(), 1);
    let project_dir = dest.join("example.com__widget");
    assert!(project_dir.join("src/main.gos").exists());
    let _ = std::fs::remove_dir_all(&dest);
}
