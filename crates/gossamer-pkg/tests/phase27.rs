//!: manifest, MVS resolver, lockfile, scaffolders.

use gossamer_pkg::{
    CaretRange, DependencySpec, InlineDependency, Lockfile, Manifest, ManifestError, ProjectId,
    ProjectIdError, Resolved, ResolvedSource, Resolver, Version, VersionCatalogue, add_registry,
    pin_to_resolved, remove, render_initial_manifest, render_main_source, tidy,
};

#[test]
fn project_id_round_trips_canonical_examples() {
    let id = ProjectId::parse("example.com/math").unwrap();
    assert_eq!(id.as_str(), "example.com/math");
    assert_eq!(id.domain(), "example.com");
    assert_eq!(id.path(), "math");
    assert_eq!(id.tail(), "math");
}

#[test]
fn project_id_rejects_short_or_malformed_inputs() {
    assert!(matches!(
        ProjectId::parse("math").unwrap_err(),
        ProjectIdError::InvalidDomain(_)
    ));
    assert!(matches!(
        ProjectId::parse("Foo.com/bar").unwrap_err(),
        ProjectIdError::InvalidDomain(_)
    ));
    assert!(matches!(
        ProjectId::parse("example.com/Bad").unwrap_err(),
        ProjectIdError::InvalidSegment(_)
    ));
    assert!(matches!(
        ProjectId::parse("").unwrap_err(),
        ProjectIdError::Empty
    ));
}

#[test]
fn version_parses_and_orders_lexicographically() {
    let a = Version::parse("0.1.0").unwrap();
    let b = Version::parse("0.2.0").unwrap();
    let c = Version::parse("1.0.0").unwrap();
    assert!(a < b);
    assert!(b < c);
}

#[test]
fn caret_range_handles_zero_minor_pinning() {
    let range = CaretRange::parse("^0.1.5").unwrap();
    assert!(range.matches(Version::new(0, 1, 5)));
    assert!(range.matches(Version::new(0, 1, 9)));
    assert!(!range.matches(Version::new(0, 2, 0)));

    let stable = CaretRange::parse("1.2.3").unwrap();
    assert!(stable.matches(Version::new(1, 2, 3)));
    assert!(stable.matches(Version::new(1, 9, 0)));
    assert!(!stable.matches(Version::new(2, 0, 0)));
}

#[test]
fn manifest_round_trips_through_render() {
    let source = r#"[project]
id = "example.com/math"
version = "0.3.1"
authors = ["Jane Doe <jane@example.com>"]
license = "Apache-2.0"

[dependencies]
"example.org/linalg" = "1.2.0"
"example.com/logging" = { git = "https://git.example.com/logging.git", tag = "v0.8.0" }
"example.net/internal" = { path = "../internal" }

[registries]
"example.org" = "https://registry.example.org/v1"
"#;
    let manifest = Manifest::parse(source).unwrap();
    assert_eq!(manifest.project.id.as_str(), "example.com/math");
    assert_eq!(manifest.project.version, Version::new(0, 3, 1));
    assert_eq!(
        manifest.project.authors,
        vec!["Jane Doe <jane@example.com>"]
    );
    assert_eq!(manifest.project.license, "Apache-2.0");
    assert_eq!(manifest.dependencies.len(), 3);
    assert!(manifest.registries.contains_key("example.org"));

    let rendered = manifest.render();
    let reparsed = Manifest::parse(&rendered).unwrap();
    assert_eq!(reparsed, manifest);
}

#[test]
fn manifest_rejects_missing_id_or_version() {
    let missing_id = "[project]\nversion = \"0.1.0\"\n";
    assert!(matches!(
        Manifest::parse(missing_id).unwrap_err(),
        ManifestError::MissingField("project.id")
    ));
    let missing_version = "[project]\nid = \"example.com/foo\"\n";
    assert!(matches!(
        Manifest::parse(missing_version).unwrap_err(),
        ManifestError::MissingField("project.version")
    ));
}

#[test]
fn resolver_picks_minimum_version_from_catalogue() {
    let manifest = Manifest::parse(
        r#"[project]
id = "example.com/app"
version = "0.1.0"

[dependencies]
"example.org/lib" = "1.2.0"
"#,
    )
    .unwrap();
    let mut catalogue = VersionCatalogue::new();
    let lib = ProjectId::parse("example.org/lib").unwrap();
    for (maj, min, pat) in [(1, 2, 3), (1, 2, 5), (1, 4, 0), (2, 0, 0)] {
        catalogue.add(&lib, Version::new(maj, min, pat));
    }
    let resolved = Resolver::new(catalogue).resolve(&manifest).unwrap();
    assert_eq!(resolved.len(), 1);
    match &resolved[0].pin {
        ResolvedSource::Registry(v) => assert_eq!(*v, Version::new(1, 2, 3)),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn resolver_passes_inline_dependencies_through() {
    let manifest = Manifest::parse(
        r#"[project]
id = "example.com/app"
version = "0.1.0"

[dependencies]
"example.com/local" = { path = "../local" }
"example.com/repo" = { git = "https://git/foo.git", tag = "v1" }
"#,
    )
    .unwrap();
    let resolved = Resolver::new(VersionCatalogue::new())
        .resolve(&manifest)
        .unwrap();
    assert_eq!(resolved.len(), 2);
    let by_id: std::collections::BTreeMap<_, _> = resolved
        .into_iter()
        .map(|r| (r.id.as_str().to_string(), r.pin))
        .collect();
    assert!(matches!(
        by_id["example.com/local"],
        ResolvedSource::Path(_)
    ));
    assert!(matches!(
        by_id["example.com/repo"],
        ResolvedSource::Git { .. }
    ));
}

#[test]
fn resolver_reports_unsatisfiable_ranges() {
    let manifest = Manifest::parse(
        r#"[project]
id = "example.com/app"
version = "0.1.0"

[dependencies]
"example.org/lib" = "2.0.0"
"#,
    )
    .unwrap();
    let mut catalogue = VersionCatalogue::new();
    let lib = ProjectId::parse("example.org/lib").unwrap();
    catalogue.add(&lib, Version::new(1, 9, 0));
    let err = Resolver::new(catalogue).resolve(&manifest).unwrap_err();
    assert!(format!("{err:?}").contains("Unsatisfiable"));
}

#[test]
fn lockfile_round_trips_through_render() {
    let entries = vec![
        Resolved {
            id: ProjectId::parse("example.com/a").unwrap(),
            pin: ResolvedSource::Registry(Version::new(1, 2, 3)),
        },
        Resolved {
            id: ProjectId::parse("example.com/b").unwrap(),
            pin: ResolvedSource::Git {
                url: "https://git/b.git".to_string(),
                reference: "v0.4.0".to_string(),
            },
        },
    ];
    let lock = Lockfile::from_resolved(&entries);
    let rendered = lock.render();
    let reparsed = Lockfile::parse(&rendered).unwrap();
    assert_eq!(reparsed.entries, entries);
}

#[test]
fn add_registry_inserts_or_updates_entry() {
    let mut manifest = Manifest::parse("[project]\nid = \"a.b/c\"\nversion = \"0.1.0\"\n").unwrap();
    let id = ProjectId::parse("example.org/lib").unwrap();
    let changed = add_registry(&mut manifest, &id, Version::new(1, 0, 0));
    assert!(changed);
    assert!(manifest.dependencies.contains_key(id.as_str()));
    let unchanged = add_registry(&mut manifest, &id, Version::new(1, 0, 0));
    assert!(!unchanged);
    let updated = add_registry(&mut manifest, &id, Version::new(1, 1, 0));
    assert!(updated);
    if let Some(DependencySpec::Registry(range)) = manifest.dependencies.get(id.as_str()) {
        assert_eq!(range.minimum, Version::new(1, 1, 0));
    } else {
        panic!("expected registry entry");
    }
}

#[test]
fn remove_drops_entry_when_present() {
    let mut manifest = Manifest::parse(
        "[project]\nid = \"a.b/c\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"example.org/lib\" = \"1.0.0\"\n",
    )
    .unwrap();
    let id = ProjectId::parse("example.org/lib").unwrap();
    assert!(remove(&mut manifest, &id));
    assert!(manifest.dependencies.is_empty());
    assert!(!remove(&mut manifest, &id));
}

#[test]
fn tidy_keeps_only_resolved_entries() {
    let mut manifest = Manifest::parse(
        "[project]\nid = \"a.b/c\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"example.org/keep\" = \"1.0.0\"\n\"example.org/drop\" = \"2.0.0\"\n",
    )
    .unwrap();
    let kept = vec![Resolved {
        id: ProjectId::parse("example.org/keep").unwrap(),
        pin: ResolvedSource::Registry(Version::new(1, 0, 0)),
    }];
    tidy(&mut manifest, &kept);
    assert!(manifest.dependencies.contains_key("example.org/keep"));
    assert!(!manifest.dependencies.contains_key("example.org/drop"));
}

#[test]
fn pin_to_resolved_updates_registry_minimum() {
    let mut manifest = Manifest::parse(
        "[project]\nid = \"a.b/c\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"example.org/lib\" = \"1.0.0\"\n",
    )
    .unwrap();
    let resolved = Resolved {
        id: ProjectId::parse("example.org/lib").unwrap(),
        pin: ResolvedSource::Registry(Version::new(1, 4, 2)),
    };
    pin_to_resolved(&mut manifest, &resolved);
    if let Some(DependencySpec::Registry(range)) = manifest.dependencies.get("example.org/lib") {
        assert_eq!(range.minimum, Version::new(1, 4, 2));
    } else {
        panic!("expected registry entry");
    }
}

#[test]
fn scaffold_renders_initial_manifest_and_main_source() {
    let id = ProjectId::parse("example.com/widget").unwrap();
    let manifest = render_initial_manifest(&id, Version::new(0, 1, 0));
    assert!(manifest.contains("id = \"example.com/widget\""));
    assert!(manifest.contains("version = \"0.1.0\""));
    let main = render_main_source(&id);
    assert!(main.contains("hello from widget"));
    // The scaffolded manifest should round-trip through the parser.
    let parsed = Manifest::parse(&manifest).unwrap();
    assert_eq!(parsed.project.id.as_str(), "example.com/widget");
}

#[test]
fn ambiguous_inline_dependency_is_rejected() {
    let source = "[project]\nid = \"a.b/c\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"x.y/z\" = { path = \"../z\", git = \"https://git\" }\n";
    let err = Manifest::parse(source).unwrap_err();
    assert!(matches!(err, ManifestError::AmbiguousDependency(ref id) if id == "x.y/z"));
    let _: InlineDependency; // ensure type re-export compiles
}
