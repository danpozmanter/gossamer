//! — incremental build graph.

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use gossamer_driver::{
    BuildCache, BuildGraph, Crate, LinkerOptions, Profile, TargetTriple, build_workspace,
    fingerprint_all, timed,
};

fn scratch(tag: &str) -> PathBuf {
    let mut dir = env::temp_dir();
    dir.push(format!("gos-build-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn simple_graph() -> BuildGraph {
    BuildGraph {
        crates: vec![
            Crate {
                name: "leaf".to_string(),
                sources: vec![(
                    "src/lib.gos".to_string(),
                    "fn helper() -> i64 { 42i64 }\n".to_string(),
                )],
                deps: Vec::new(),
            },
            Crate {
                name: "app".to_string(),
                sources: vec![(
                    "src/main.gos".to_string(),
                    "fn main() -> i64 { 0i64 }\n".to_string(),
                )],
                deps: vec!["leaf".to_string()],
            },
        ],
        target: TargetTriple::host(),
        profile: Profile::Debug,
        toolchain: env!("CARGO_PKG_VERSION").to_string(),
    }
}

#[test]
fn first_build_is_a_full_compile_second_is_all_cache_hits() {
    let dir = scratch("hit");
    let cache = BuildCache::new(dir.clone());
    let graph = simple_graph();
    let options = LinkerOptions::default();

    let first = build_workspace(&graph, &cache, &options).unwrap();
    assert_eq!(first.len(), 2);
    assert!(first.iter().all(|o| !o.from_cache));

    let second = build_workspace(&graph, &cache, &options).unwrap();
    assert!(second.iter().all(|o| o.from_cache));
    assert_eq!(
        first
            .iter()
            .map(|o| o.fingerprint.clone())
            .collect::<Vec<_>>(),
        second
            .iter()
            .map(|o| o.fingerprint.clone())
            .collect::<Vec<_>>(),
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn touching_leaf_forces_every_downstream_crate_to_recompile() {
    let dir = scratch("leaf-bump");
    let cache = BuildCache::new(dir.clone());
    let mut graph = simple_graph();
    let options = LinkerOptions::default();

    let _ = build_workspace(&graph, &cache, &options).unwrap();
    graph.crates[0].sources[0].1 = "fn helper() -> i64 { 43i64 }\n".to_string();
    let after = build_workspace(&graph, &cache, &options).unwrap();

    let leaf = after.iter().find(|o| o.crate_name == "leaf").unwrap();
    let app = after.iter().find(|o| o.crate_name == "app").unwrap();
    assert!(!leaf.from_cache, "leaf must rebuild");
    assert!(
        !app.from_cache,
        "app must rebuild because leaf's fingerprint moved"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn touching_app_only_rebuilds_app() {
    let dir = scratch("app-bump");
    let cache = BuildCache::new(dir.clone());
    let mut graph = simple_graph();
    let options = LinkerOptions::default();

    let _ = build_workspace(&graph, &cache, &options).unwrap();
    graph.crates[1].sources[0].1 = "fn main() -> i64 { 7i64 }\n".to_string();
    let after = build_workspace(&graph, &cache, &options).unwrap();

    let leaf = after.iter().find(|o| o.crate_name == "leaf").unwrap();
    let app = after.iter().find(|o| o.crate_name == "app").unwrap();
    assert!(leaf.from_cache, "leaf is untouched and should be cached");
    assert!(!app.from_cache, "app was edited and must rebuild");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_op_rebuild_completes_within_the_phase_29_budget() {
    let dir = scratch("noop");
    let cache = BuildCache::new(dir.clone());
    let graph = simple_graph();
    let options = LinkerOptions::default();

    let _ = build_workspace(&graph, &cache, &options).unwrap();
    let (second, elapsed) = timed(|| build_workspace(&graph, &cache, &options).unwrap());
    assert!(second.iter().all(|o| o.from_cache));
    assert!(
        elapsed < Duration::from_millis(500),
        "no-op took {elapsed:?}, budget 500ms (plan target 50ms)"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fingerprints_partition_cleanly_per_target() {
    // Pick a cross triple that is guaranteed to differ from the
    // host. A hard-coded `aarch64-apple-darwin` collides with the
    // host on Apple Silicon CI runners — pick the opposite arch
    // for that case.
    let mut graph = simple_graph();
    let host = TargetTriple::host();
    let cross = if host.as_str() == "aarch64-apple-darwin" {
        TargetTriple("x86_64-unknown-linux-gnu".to_string())
    } else {
        TargetTriple("aarch64-apple-darwin".to_string())
    };
    assert_ne!(host.as_str(), cross.as_str(), "test setup picked the host");
    let host_fps = fingerprint_all(&graph).unwrap();
    graph.target = cross;
    let cross_fps = fingerprint_all(&graph).unwrap();
    assert_ne!(host_fps.get("leaf"), cross_fps.get("leaf"));
    assert_ne!(host_fps.get("app"), cross_fps.get("app"));
}

#[test]
fn parallel_level_compiles_multiple_independent_crates() {
    let dir = scratch("parallel");
    let cache = BuildCache::new(dir.clone());
    let graph = BuildGraph {
        crates: (0..4)
            .map(|i| Crate {
                name: format!("leaf{i}"),
                sources: vec![(
                    format!("src/leaf{i}.gos"),
                    format!("fn val() -> i64 {{ {i}i64 }}\n"),
                )],
                deps: Vec::new(),
            })
            .collect(),
        target: TargetTriple::host(),
        profile: Profile::Debug,
        toolchain: "parallel-test".to_string(),
    };
    let options = LinkerOptions::default();
    let outputs = build_workspace(&graph, &cache, &options).unwrap();
    assert_eq!(outputs.len(), 4);
    assert!(outputs.iter().all(|o| !o.from_cache));
    let names: Vec<&str> = outputs.iter().map(|o| o.crate_name.as_str()).collect();
    for expected in ["leaf0", "leaf1", "leaf2", "leaf3"] {
        assert!(names.contains(&expected));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
