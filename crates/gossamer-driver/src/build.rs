//! Workspace build graph and incremental-build cache.
//! Each build node is a `Crate × Target × Profile` triple. A
//! fingerprint is computed over:
//! - crate name,
//! - profile,
//! - target triple,
//! - toolchain version (`CARGO_PKG_VERSION`),
//! - sorted `(path, sha256-of-bytes)` pairs for every source file,
//! - sorted fingerprints of upstream dependencies.
//!
//! Compiled artifacts are cached on disk under
//! `<root>/<target>/<fingerprint>/artifact.bin`. A second build with
//! identical inputs short-circuits to a cache read.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use gossamer_pkg::sha256;
use thiserror::Error;

use crate::link::{Artifact, LinkerOptions, TargetTriple};
use crate::pipeline::compile_source;

/// Release / debug switch. Profiles that produce bit-identical output
/// for the same source still participate in fingerprinting so cache
/// hits between them are impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    /// Unoptimised, fast-to-build profile.
    Debug,
    /// Optimised release profile.
    Release,
}

impl Profile {
    /// Returns the profile's short textual tag.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }
}

/// A single compilable unit inside a build graph.
#[derive(Debug, Clone)]
pub struct Crate {
    /// Human-readable crate name (used for diagnostics and mangling).
    pub name: String,
    /// Source files belonging to this crate, each paired with its
    /// contents. The first entry is the crate root (the file that
    /// contains `fn main`); every subsequent entry is auto-bundled
    /// as an inline `mod <stem> { ... }` so cross-file calls resolve
    /// without a manual `mod` declaration in the entry source.
    pub sources: Vec<(String, String)>,
    /// Upstream crate names this crate depends on.
    pub deps: Vec<String>,
}

/// Collection of crates scheduled for a single workspace build.
#[derive(Debug, Clone)]
pub struct BuildGraph {
    /// Crates participating in the build, in any order.
    pub crates: Vec<Crate>,
    /// Target triple for the whole workspace.
    pub target: TargetTriple,
    /// Build profile.
    pub profile: Profile,
    /// Toolchain identifier - typically `CARGO_PKG_VERSION` of the
    /// driver crate.
    pub toolchain: String,
}

/// Result of compiling (or loading from cache) a single crate.
#[derive(Debug, Clone)]
pub struct BuildOutput {
    /// Crate name the record describes.
    pub crate_name: String,
    /// Cache key used for this crate.
    pub fingerprint: String,
    /// Compiled artifact bytes.
    pub bytes: Vec<u8>,
    /// `true` if the artifact was served from the cache without
    /// recompiling.
    pub from_cache: bool,
}

/// Errors raised while walking or compiling a build graph.
#[derive(Debug, Error)]
pub enum BuildError {
    /// The graph contains a crate that refers to a non-existent
    /// dependency.
    #[error("crate `{crate_name}` depends on unknown crate `{missing}`")]
    UnknownDependency {
        /// Crate that holds the dangling reference.
        crate_name: String,
        /// Name that failed to resolve.
        missing: String,
    },
    /// The graph contains a cycle.
    #[error("dependency cycle involving `{culprit}`")]
    Cycle {
        /// One of the crates caught in the cycle.
        culprit: String,
    },
    /// Underlying I/O failure during cache read/write.
    #[error("cache I/O failed at {path}: {source}")]
    Io {
        /// Path the driver was interacting with.
        path: PathBuf,
        /// Wrapped I/O error.
        #[source]
        source: io::Error,
    },
    /// Worker thread panicked during parallel compilation.
    #[error("worker thread panicked: {message}")]
    Worker {
        /// Best-effort panic message.
        message: String,
    },
}

impl BuildError {
    fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// On-disk cache rooted at an arbitrary directory. The production
/// location is `~/.gossamer/build/`; tests use a scratch directory.
#[derive(Debug, Clone)]
pub struct BuildCache {
    root: PathBuf,
}

impl BuildCache {
    /// Creates a cache rooted at `root`. The directory is created
    /// lazily on first write.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Cache rooted at `$HOME/.gossamer/build`, or the process-local
    /// `.gossamer/build` directory when `HOME` is unset.
    #[must_use]
    pub fn user_default() -> Self {
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
        Self::new(home.join(".gossamer").join("build"))
    }

    /// Root directory of the cache.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn slot(&self, target: &TargetTriple, fingerprint: &str) -> PathBuf {
        self.root.join(target.as_str()).join(fingerprint)
    }

    /// Returns the cached artifact bytes, if any.
    #[must_use]
    pub fn read(&self, target: &TargetTriple, fingerprint: &str) -> Option<Vec<u8>> {
        fs::read(self.slot(target, fingerprint).join("artifact.bin")).ok()
    }

    /// Writes `bytes` into the cache slot for `(target, fingerprint)`.
    pub fn write(
        &self,
        target: &TargetTriple,
        fingerprint: &str,
        bytes: &[u8],
    ) -> Result<(), BuildError> {
        let dir = self.slot(target, fingerprint);
        fs::create_dir_all(&dir).map_err(|e| BuildError::io(&dir, e))?;
        let path = dir.join("artifact.bin");
        fs::write(&path, bytes).map_err(|e| BuildError::io(&path, e))?;
        Ok(())
    }
}

/// Computes the fingerprint for a single crate given the fingerprints
/// of its dependencies (already-computed). Exposed so callers can
/// inspect the digest without triggering a compile.
#[must_use]
pub fn fingerprint(
    graph: &BuildGraph,
    krate: &Crate,
    dep_fingerprints: &BTreeMap<String, String>,
) -> String {
    let mut hasher = Accumulator::default();
    hasher.push_str("name");
    hasher.push_str(&krate.name);
    hasher.push_str("profile");
    hasher.push_str(graph.profile.as_str());
    hasher.push_str("target");
    hasher.push_str(graph.target.as_str());
    hasher.push_str("toolchain");
    hasher.push_str(&graph.toolchain);

    let mut sources: Vec<_> = krate.sources.iter().collect();
    sources.sort_by(|a, b| a.0.cmp(&b.0));
    hasher.push_str("sources");
    for (path, body) in sources {
        hasher.push_str(path);
        hasher.push_str(&sha256::hex(body.as_bytes()));
    }

    let mut deps: Vec<_> = krate.deps.iter().collect();
    deps.sort();
    hasher.push_str("deps");
    for dep in deps {
        hasher.push_str(dep);
        let dep_fp = dep_fingerprints
            .get(dep)
            .map(String::as_str)
            .unwrap_or_default();
        hasher.push_str(dep_fp);
    }

    sha256::hex(hasher.buffer())
}

#[derive(Default)]
struct Accumulator {
    buf: Vec<u8>,
}

impl Accumulator {
    fn push_str(&mut self, s: &str) {
        let bytes = s.as_bytes();
        let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        self.buf.extend_from_slice(&len.to_le_bytes());
        self.buf.extend_from_slice(bytes);
    }

    fn buffer(&self) -> &[u8] {
        &self.buf
    }
}

/// Computes fingerprints for every crate in `graph`, in topological
/// order. Returns the per-crate fingerprint map.
pub fn fingerprint_all(graph: &BuildGraph) -> Result<BTreeMap<String, String>, BuildError> {
    let order = topological_order(graph)?;
    let mut fingerprints: BTreeMap<String, String> = BTreeMap::new();
    for idx in order {
        let krate = &graph.crates[idx];
        let fp = fingerprint(graph, krate, &fingerprints);
        fingerprints.insert(krate.name.clone(), fp);
    }
    Ok(fingerprints)
}

fn topological_order(graph: &BuildGraph) -> Result<Vec<usize>, BuildError> {
    let index: BTreeMap<&str, usize> = graph
        .crates
        .iter()
        .enumerate()
        .map(|(i, c)| (c.name.as_str(), i))
        .collect();

    for krate in &graph.crates {
        for dep in &krate.deps {
            if !index.contains_key(dep.as_str()) {
                return Err(BuildError::UnknownDependency {
                    crate_name: krate.name.clone(),
                    missing: dep.clone(),
                });
            }
        }
    }

    let n = graph.crates.len();
    let mut order = Vec::with_capacity(n);
    let mut state = vec![NodeState::White; n];
    for start in 0..n {
        visit(start, graph, &index, &mut state, &mut order)?;
    }
    Ok(order)
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum NodeState {
    White,
    Grey,
    Black,
}

fn visit(
    idx: usize,
    graph: &BuildGraph,
    index: &BTreeMap<&str, usize>,
    state: &mut [NodeState],
    order: &mut Vec<usize>,
) -> Result<(), BuildError> {
    match state[idx] {
        NodeState::Black => return Ok(()),
        NodeState::Grey => {
            return Err(BuildError::Cycle {
                culprit: graph.crates[idx].name.clone(),
            });
        }
        NodeState::White => {}
    }
    state[idx] = NodeState::Grey;
    for dep in &graph.crates[idx].deps {
        let dep_idx = index[dep.as_str()];
        visit(dep_idx, graph, index, state, order)?;
    }
    state[idx] = NodeState::Black;
    order.push(idx);
    Ok(())
}

/// Groups a topological order into level-sets. All crates in one level
/// can be compiled in parallel because every dependency sits in a
/// strictly earlier level.
fn schedule_levels(graph: &BuildGraph, order: &[usize]) -> Vec<Vec<usize>> {
    let mut level_of: Vec<u32> = vec![0; graph.crates.len()];
    let dep_index: BTreeMap<&str, usize> = graph
        .crates
        .iter()
        .enumerate()
        .map(|(i, c)| (c.name.as_str(), i))
        .collect();
    for &idx in order {
        let mut highest: i64 = -1;
        for dep in &graph.crates[idx].deps {
            let dep_idx = dep_index[dep.as_str()];
            highest = highest.max(i64::from(level_of[dep_idx]));
        }
        level_of[idx] = u32::try_from(highest + 1).unwrap_or(0);
    }
    let max_level = level_of.iter().copied().max().unwrap_or(0);
    let mut levels: Vec<Vec<usize>> = (0..=max_level).map(|_| Vec::new()).collect();
    for &idx in order {
        let lvl = level_of[idx] as usize;
        levels[lvl].push(idx);
    }
    levels
}

/// Orchestrates a full workspace compile. Each crate is either served
/// from `cache` or freshly compiled; independent crates inside the same
/// topological level are compiled in parallel.
pub fn build_workspace(
    graph: &BuildGraph,
    cache: &BuildCache,
    options: &LinkerOptions,
) -> Result<Vec<BuildOutput>, BuildError> {
    let order = topological_order(graph)?;
    let levels = schedule_levels(graph, &order);

    let mut fingerprints: BTreeMap<String, String> = BTreeMap::new();
    let mut outputs_by_name: BTreeMap<String, BuildOutput> = BTreeMap::new();
    let options = Arc::new(options.clone());

    for level in levels {
        let work: Vec<WorkItem> = level
            .iter()
            .map(|&idx| {
                let krate = graph.crates[idx].clone();
                let fp = fingerprint(graph, &krate, &fingerprints);
                WorkItem {
                    krate,
                    fingerprint: fp,
                }
            })
            .collect();

        for item in &work {
            fingerprints.insert(item.krate.name.clone(), item.fingerprint.clone());
        }

        let target = graph.target.clone();
        let results = run_level(work, cache, &target, &options)?;
        for output in results {
            outputs_by_name.insert(output.crate_name.clone(), output);
        }
    }

    let ordered: Vec<BuildOutput> = order
        .into_iter()
        .filter_map(|idx| outputs_by_name.remove(&graph.crates[idx].name))
        .collect();
    Ok(ordered)
}

struct WorkItem {
    krate: Crate,
    fingerprint: String,
}

fn run_level(
    work: Vec<WorkItem>,
    cache: &BuildCache,
    target: &TargetTriple,
    options: &Arc<LinkerOptions>,
) -> Result<Vec<BuildOutput>, BuildError> {
    if work.len() <= 1 {
        let mut out = Vec::with_capacity(work.len());
        for item in work {
            out.push(compile_one(&item, cache, target, options)?);
        }
        return Ok(out);
    }

    let cache_root = cache.root().to_path_buf();
    let target = target.clone();
    let options = Arc::clone(options);
    let mut handles = Vec::with_capacity(work.len());
    for item in work {
        let cache = BuildCache::new(cache_root.clone());
        let target = target.clone();
        let options = Arc::clone(&options);
        handles.push(thread::spawn(move || {
            compile_one(&item, &cache, &target, &options)
        }));
    }
    let mut out = Vec::with_capacity(handles.len());
    for handle in handles {
        match handle.join() {
            Ok(result) => out.push(result?),
            Err(panic) => {
                let message = panic_message(&panic);
                return Err(BuildError::Worker { message });
            }
        }
    }
    Ok(out)
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

fn compile_one(
    item: &WorkItem,
    cache: &BuildCache,
    target: &TargetTriple,
    options: &LinkerOptions,
) -> Result<BuildOutput, BuildError> {
    if let Some(bytes) = cache.read(target, &item.fingerprint) {
        return Ok(BuildOutput {
            crate_name: item.krate.name.clone(),
            fingerprint: item.fingerprint.clone(),
            bytes,
            from_cache: true,
        });
    }
    let source = bundle_crate_sources(&item.krate.sources);
    let artifact: Artifact = compile_source(&source, &item.krate.name, options);
    cache.write(target, &item.fingerprint, &artifact.bytes)?;
    Ok(BuildOutput {
        crate_name: item.krate.name.clone(),
        fingerprint: item.fingerprint.clone(),
        bytes: artifact.bytes,
        from_cache: false,
    })
}

/// Bundles every source in a crate into a single string suitable for
/// the compiler pipeline. The first source is the crate entry; every
/// remaining source is wrapped as `mod <stem> { ... }` and appended so
/// cross-file calls (`util::helper()` in `main.gos` referencing
/// `src/util.gos::helper`) resolve. Matches the CLI's sibling
/// auto-bundle contract so single-file and workspace builds produce
/// the same merged source.
///
/// Sources whose path stem is not a valid module identifier, or that
/// start with `_` / end in `_test`, are skipped (mirroring the CLI
/// rule). If a sibling stem appears in the entry source as a bare
/// `mod NAME;` declaration, that line is commented out so the
/// auto-bundled body is the sole definition.
#[must_use]
pub fn bundle_crate_sources(sources: &[(String, String)]) -> String {
    let Some(((entry_path, entry_body), rest)) = sources.split_first() else {
        return String::new();
    };
    let entry_stem = path_stem(entry_path);
    let mut siblings: Vec<(&str, &str)> = Vec::new();
    for (path, body) in rest {
        let stem = path_stem(path);
        if stem.is_empty()
            || stem == entry_stem
            || stem.starts_with('_')
            || stem.ends_with("_test")
            || !is_valid_module_ident(stem)
        {
            continue;
        }
        siblings.push((stem, body.as_str()));
    }
    if siblings.is_empty() {
        return entry_body.clone();
    }
    siblings.sort_by(|a, b| a.0.cmp(b.0));
    let stems: Vec<&str> = siblings.iter().map(|(n, _)| *n).collect();
    let mut out = neutralize_external_mod_decls(entry_body, &stems);
    for (stem, body) in &siblings {
        out.push('\n');
        out.push_str("// auto-bundled module: ");
        out.push_str(stem);
        out.push('\n');
        out.push_str("mod ");
        out.push_str(stem);
        out.push_str(" {\n");
        out.push_str(body);
        out.push_str("\n}\n");
    }
    out
}

fn path_stem(path: &str) -> &str {
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    base.strip_suffix(".gos").unwrap_or(base)
}

fn is_valid_module_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn neutralize_external_mod_decls(source: &str, sibling_stems: &[&str]) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("mod ")
            && let Some(name) = rest.strip_suffix(';')
            && sibling_stems.contains(&name.trim())
        {
            out.push_str("// (auto-bundled) ");
            out.push_str(line);
            continue;
        }
        out.push_str(line);
    }
    out
}

/// Measures how long `closure` takes to run. Used by the no-op
/// incremental-build test to assert the 50 ms budget.
pub fn timed<T>(closure: impl FnOnce() -> T) -> (T, std::time::Duration) {
    let start = Instant::now();
    let value = closure();
    (value, start.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_graph() -> BuildGraph {
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
            toolchain: "test".to_string(),
        }
    }

    #[test]
    fn topo_order_places_leaf_before_app() {
        let graph = sample_graph();
        let order = topological_order(&graph).unwrap();
        let names: Vec<&str> = order
            .iter()
            .map(|&i| graph.crates[i].name.as_str())
            .collect();
        assert_eq!(names, vec!["leaf", "app"]);
    }

    #[test]
    fn fingerprint_is_stable_across_calls() {
        let graph = sample_graph();
        let a = fingerprint_all(&graph).unwrap();
        let b = fingerprint_all(&graph).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.get("leaf").unwrap().len(), 64);
    }

    #[test]
    fn editing_leaf_source_changes_every_downstream_fingerprint() {
        let mut graph = sample_graph();
        let before = fingerprint_all(&graph).unwrap();
        graph.crates[0].sources[0].1 = "fn helper() -> i64 { 43i64 }\n".to_string();
        let after = fingerprint_all(&graph).unwrap();
        assert_ne!(before.get("leaf"), after.get("leaf"));
        assert_ne!(before.get("app"), after.get("app"));
    }

    #[test]
    fn editing_app_source_does_not_change_leaf_fingerprint() {
        let mut graph = sample_graph();
        let before = fingerprint_all(&graph).unwrap();
        graph.crates[1].sources[0].1 = "fn main() -> i64 { 7i64 }\n".to_string();
        let after = fingerprint_all(&graph).unwrap();
        assert_eq!(before.get("leaf"), after.get("leaf"));
        assert_ne!(before.get("app"), after.get("app"));
    }

    #[test]
    fn unknown_dependency_is_reported() {
        let graph = BuildGraph {
            crates: vec![Crate {
                name: "app".to_string(),
                sources: vec![("x".to_string(), "fn main() {}\n".to_string())],
                deps: vec!["missing".to_string()],
            }],
            target: TargetTriple::host(),
            profile: Profile::Debug,
            toolchain: "t".to_string(),
        };
        let err = topological_order(&graph).unwrap_err();
        assert!(matches!(err, BuildError::UnknownDependency { .. }));
    }

    #[test]
    fn cycle_is_reported() {
        let graph = BuildGraph {
            crates: vec![
                Crate {
                    name: "a".to_string(),
                    sources: Vec::new(),
                    deps: vec!["b".to_string()],
                },
                Crate {
                    name: "b".to_string(),
                    sources: Vec::new(),
                    deps: vec!["a".to_string()],
                },
            ],
            target: TargetTriple::host(),
            profile: Profile::Debug,
            toolchain: "t".to_string(),
        };
        let err = topological_order(&graph).unwrap_err();
        assert!(matches!(err, BuildError::Cycle { .. }));
    }

    #[test]
    fn bundle_multi_file_wraps_non_entry_as_mod() {
        let sources = vec![
            (
                "src/main.gos".to_string(),
                "fn main() -> i64 { util::ten() }\n".to_string(),
            ),
            (
                "src/util.gos".to_string(),
                "pub fn ten() -> i64 { 10 }\n".to_string(),
            ),
        ];
        let bundled = bundle_crate_sources(&sources);
        assert!(bundled.contains("fn main()"));
        assert!(bundled.contains("mod util {"));
        assert!(bundled.contains("pub fn ten()"));
    }

    #[test]
    fn bundle_single_file_returns_entry_unchanged() {
        let sources = vec![("src/main.gos".to_string(), "fn main() {}\n".to_string())];
        let bundled = bundle_crate_sources(&sources);
        assert_eq!(bundled, "fn main() {}\n");
    }

    #[test]
    fn bundle_neutralizes_external_mod_decl() {
        let sources = vec![
            (
                "src/main.gos".to_string(),
                "mod util;\nfn main() {}\n".to_string(),
            ),
            (
                "src/util.gos".to_string(),
                "pub fn ten() -> i64 { 10 }\n".to_string(),
            ),
        ];
        let bundled = bundle_crate_sources(&sources);
        assert!(bundled.contains("// (auto-bundled) mod util;"));
        assert!(bundled.contains("mod util {"));
    }

    #[test]
    fn bundle_skips_underscore_and_test_files() {
        let sources = vec![
            ("src/main.gos".to_string(), "fn main() {}\n".to_string()),
            ("src/_internal.gos".to_string(), "fn x() {}\n".to_string()),
            ("src/foo_test.gos".to_string(), "fn y() {}\n".to_string()),
        ];
        let bundled = bundle_crate_sources(&sources);
        assert!(!bundled.contains("mod _internal"));
        assert!(!bundled.contains("mod foo_test"));
    }

    #[test]
    fn schedule_levels_groups_independents_together() {
        let graph = BuildGraph {
            crates: vec![
                Crate {
                    name: "a".to_string(),
                    sources: Vec::new(),
                    deps: Vec::new(),
                },
                Crate {
                    name: "b".to_string(),
                    sources: Vec::new(),
                    deps: Vec::new(),
                },
                Crate {
                    name: "c".to_string(),
                    sources: Vec::new(),
                    deps: vec!["a".to_string(), "b".to_string()],
                },
            ],
            target: TargetTriple::host(),
            profile: Profile::Debug,
            toolchain: "t".to_string(),
        };
        let order = topological_order(&graph).unwrap();
        let levels = schedule_levels(&graph, &order);
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].len(), 2);
        assert_eq!(levels[1].len(), 1);
    }
}
