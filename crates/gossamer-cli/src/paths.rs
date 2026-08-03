//! Path-resolution + filesystem helpers shared by every subcommand.
//!
//! Centralising these here keeps `main.rs` minimal and gives every
//! subcommand the same project-root / source-discovery behaviour.

use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use gossamer_pkg::{Manifest, find_manifest};

/// `true` when ANSI colour escapes should be written to stderr.
/// Honours `NO_COLOR` and `CLICOLOR=0`; otherwise tests for a TTY.
pub(crate) fn stderr_supports_colour() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if matches!(std::env::var("CLICOLOR").as_deref(), Ok("0")) {
        return false;
    }
    std::io::stderr().is_terminal()
}

/// Reads `file` (after `.gos`-resolution) into a `String`. Wraps
/// the OS error in [`friendly_io_error`] so the diagnostic stream
/// stays free of libc artefacts.
pub(crate) fn read_source(file: &Path) -> Result<String> {
    let resolved = resolve_gos_source(file);
    fs::read_to_string(&resolved).map_err(|err| friendly_io_error(err, &resolved))
}

/// Reads `file` and auto-bundles every sibling `*.gos` in the same
/// directory by wrapping each in `mod NAME { ... }` and appending
/// it to the entry source. Used by entry-point commands
/// (`gos`, `gos build`) so cross-module calls
/// (`other::greet()` in `main.gos` referencing
/// `src/other.gos::greet`) resolve at runtime. See
/// [`bundle_sibling_modules`] for the bundling contract.
pub(crate) fn read_entry_source(file: &Path) -> Result<String> {
    // A bare relative entry (`gos run main.gos`) has an empty
    // `parent()`; the module scan must read the entry's real
    // directory, so anchor the path to the cwd first.
    let resolved =
        std::path::absolute(resolve_gos_source(file)).unwrap_or_else(|_| resolve_gos_source(file));
    let entry = fs::read_to_string(&resolved).map_err(|err| friendly_io_error(err, &resolved))?;
    let bundled = bundle_sibling_modules(&resolved, entry);
    let mut visited = Vec::new();
    Ok(bundle_path_dependencies(&resolved, bundled, &mut visited))
}

/// Inlines every `path = "..."` dependency of the entry's project as
/// a top-level `mod <dep-module-name> { <dep source> }`. Transitive
/// path dependencies hoist to top-level modules too (deduplicated by
/// canonical root), so a dependency's own `use "id" as alias` binds
/// against a sibling module exactly as the consumer's does. The
/// module name derives from the dependency's project id via
/// `gossamer_resolve::project_dep_module_name`, which is also what
/// the resolver binds `use "id" as alias` against. Non-path
/// dependencies are untouched.
fn bundle_path_dependencies(entry: &Path, source: String, visited: &mut Vec<PathBuf>) -> String {
    let mut out = source;
    let mut worklist: Vec<(PathBuf, PathBuf)> = Vec::new();
    collect_path_deps(entry, visited, &mut worklist);
    let mut i = 0;
    while i < worklist.len() {
        let (dep_root, dep_entry) = worklist[i].clone();
        i += 1;
        let Some((dep_id, _)) = path_dep_entry(&dep_root) else {
            continue;
        };
        let Ok(dep_source) = fs::read_to_string(&dep_entry) else {
            continue;
        };
        let dep_bundled = bundle_sibling_modules(&dep_entry, dep_source);
        collect_path_deps(&dep_entry, visited, &mut worklist);
        let mod_name = gossamer_resolve::project_dep_module_name(&dep_id);
        out.push_str(&format!(
            "\n// auto-bundled dependency: {} ({})\nmod {} {{\n{}\n}}\n",
            dep_id,
            dep_root.display(),
            mod_name,
            dep_bundled
        ));
    }
    out
}

/// Appends the (root, entry) of each not-yet-visited path dependency
/// of `entry`'s project to `worklist`.
fn collect_path_deps(
    entry: &Path,
    visited: &mut Vec<PathBuf>,
    worklist: &mut Vec<(PathBuf, PathBuf)>,
) {
    let Some(dir) = entry.parent() else {
        return;
    };
    let manifest_path = [dir.join("project.toml")]
        .into_iter()
        .chain(dir.parent().map(|p| p.join("project.toml")))
        .find(|p| p.is_file());
    let Some(manifest_path) = manifest_path else {
        return;
    };
    let Ok(manifest_text) = fs::read_to_string(&manifest_path) else {
        return;
    };
    let Ok(manifest) = gossamer_pkg::Manifest::parse(&manifest_text) else {
        return;
    };
    let manifest_dir = manifest_path.parent().unwrap_or(dir);
    for spec in manifest.dependencies.values() {
        let Some(rel) = dependency_path(spec) else {
            continue;
        };
        let Ok(dep_root) = manifest_dir.join(rel).canonicalize() else {
            continue;
        };
        if visited.contains(&dep_root) {
            continue;
        }
        visited.push(dep_root.clone());
        if let Some((_, dep_entry)) = path_dep_entry(&dep_root) {
            worklist.push((dep_root, dep_entry));
        }
    }
}

/// The `path` field of a dependency spec, when it is a local-path
/// dependency.
fn dependency_path(spec: &gossamer_pkg::DependencySpec) -> Option<&str> {
    match spec {
        gossamer_pkg::DependencySpec::Inline(gossamer_pkg::InlineDependency::Path { path }) => {
            Some(path)
        }
        _ => None,
    }
}

/// Resolves a path dependency's project id and entry source file:
/// `lib.gos` (flat or under `src/`) is the library entry; `main.gos`
/// is accepted as a fallback for binary-shaped projects.
fn path_dep_entry(dep_root: &Path) -> Option<(String, PathBuf)> {
    let manifest_text = fs::read_to_string(dep_root.join("project.toml")).ok()?;
    let manifest = gossamer_pkg::Manifest::parse(&manifest_text).ok()?;
    let entry = manifest
        .project
        .entry
        .as_deref()
        .map(|rel| dep_root.join(rel))
        .into_iter()
        .chain([
            dep_root.join("src/lib.gos"),
            dep_root.join("lib.gos"),
            dep_root.join("src/main.gos"),
            dep_root.join("main.gos"),
        ])
        .find(|p| p.is_file())?;
    Some((manifest.project.id.as_str().to_string(), entry))
}

/// Auto-bundles a multi-file package into the entry source so the
/// resolver sees one inline module tree. Every sibling `*.gos` file in
/// the entry's directory becomes `mod <stem> { ... }`, and every
/// subdirectory holding a `mod.gos` becomes `mod <dir> { ... }` whose
/// body is that `mod.gos` plus its own files and subdirectories,
/// recursively. The entry file itself, `_`-prefixed scratch files, and
/// `*_test.gos` files are skipped. A `mod NAME;` declaration for a
/// bundled module is rewritten to a comment so the synthetic inline
/// body is the sole definition.
///
/// This is the "sibling auto-bundle" contract: items inside
/// `src/<name>.gos` are reachable from `src/main.gos` as `name::item`,
/// items inside `src/<dir>/mod.gos` as `dir::item`, and a module
/// reaches a sibling module via `super::sibling::item`.
pub(crate) fn bundle_sibling_modules(entry: &Path, source: String) -> String {
    let Some(dir) = entry.parent() else {
        return source;
    };
    // Auto-bundle only fires inside an actual project - i.e. when a
    // `project.toml` lives next to the entry's directory or one
    // level up (`<root>/src/main.gos` is the canonical case). Loose
    // single-file `gos run /tmp/foo.gos` invocations must NOT pick
    // up unrelated `.gos` files sitting in the same directory.
    if !is_inside_project(dir) {
        return source;
    }
    let entry_stem = entry.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let modules = collect_package_modules(dir, Some(entry_stem));
    if modules.is_empty() {
        return source;
    }
    let names: Vec<&str> = modules.iter().map(|m| m.name.as_str()).collect();
    let mut bundled = neutralize_external_mod_decls(&source, &names);
    for module in &modules {
        append_inline_module(&mut bundled, module);
    }
    bundled
}

/// One auto-bundled module: its `name`, its already-assembled `body`,
/// and the source path it came from (for the bundle comment).
struct BundledModule {
    name: String,
    body: String,
    origin: PathBuf,
}

/// Collects the modules declared by the contents of `dir`: each
/// sibling `*.gos` file (a leaf module) and each subdirectory holding a
/// `mod.gos` (a nested module, assembled recursively). `skip_stem`
/// excludes the entry file at the top level; `mod.gos` is always
/// excluded here because it is the body of its own directory module,
/// not a sibling. Results are sorted by name for deterministic output.
fn collect_package_modules(dir: &Path, skip_stem: Option<&str>) -> Vec<BundledModule> {
    let mut modules: Vec<BundledModule> = Vec::new();
    let Ok(read) = fs::read_dir(dir) else {
        return modules;
    };
    for dirent in read.flatten() {
        let path = dirent.path();
        if path.is_file() {
            if path.extension().and_then(|s| s.to_str()) != Some("gos") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem == "mod" || Some(stem) == skip_stem {
                continue;
            }
            if stem.starts_with('_') || stem.ends_with("_test") {
                continue;
            }
            if !is_valid_module_ident(stem) {
                continue;
            }
            let Ok(body) = fs::read_to_string(&path) else {
                continue;
            };
            modules.push(BundledModule {
                name: stem.to_string(),
                body,
                origin: path,
            });
        } else if path.is_dir() {
            // A subdirectory is a module only when it carries a
            // `mod.gos` root - the documented `src/<dir>/mod.gos`
            // convention. Directories without one (e.g. `target`)
            // are ignored.
            let mod_gos = path.join("mod.gos");
            if !mod_gos.is_file() {
                continue;
            }
            let Some(dir_name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if dir_name.starts_with('_') || !is_valid_module_ident(dir_name) {
                continue;
            }
            modules.push(BundledModule {
                name: dir_name.to_string(),
                body: assemble_dir_module(&path),
                origin: mod_gos,
            });
        }
    }
    modules.sort_by(|a, b| a.name.cmp(&b.name));
    modules
}

/// Builds the body of a directory module: its `mod.gos` contents with
/// any `mod NAME;` for a child neutralized, followed by each child file
/// and subdirectory inlined as a nested module.
fn assemble_dir_module(dir: &Path) -> String {
    let root = fs::read_to_string(dir.join("mod.gos")).unwrap_or_default();
    let children = collect_package_modules(dir, None);
    let names: Vec<&str> = children.iter().map(|m| m.name.as_str()).collect();
    let mut body = neutralize_external_mod_decls(&root, &names);
    for child in &children {
        append_inline_module(&mut body, child);
    }
    body
}

/// Appends `mod <name> { <body> }` (with a provenance comment) to `out`.
fn append_inline_module(out: &mut String, module: &BundledModule) {
    out.push('\n');
    out.push_str("// auto-bundled module: ");
    out.push_str(module.origin.to_string_lossy().as_ref());
    out.push('\n');
    out.push_str("mod ");
    out.push_str(&module.name);
    out.push_str(" {\n");
    out.push_str(&module.body);
    out.push_str("\n}\n");
}

/// Comments out any line that exactly matches `mod NAME;` for one
/// of the supplied sibling stems. The regex shape is intentionally
/// narrow: only an unindented `mod NAME;` (with optional whitespace)
/// is rewritten, so a real inline `mod NAME { ... }` declaration
/// inside the entry source survives untouched.
fn neutralize_external_mod_decls(source: &str, sibling_stems: &[&str]) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("mod ") {
            if let Some(name) = rest.strip_suffix(';') {
                let name = name.trim();
                if sibling_stems.contains(&name) {
                    out.push_str("// (sibling auto-bundled) ");
                    out.push_str(line);
                    continue;
                }
            }
        }
        out.push_str(line);
    }
    out
}

/// `true` when `dir` is the source root of a Gossamer project: a
/// `project.toml` lives in `dir` itself or in `dir`'s immediate
/// parent. Used by [`bundle_sibling_modules`] to refuse bundling in
/// loose-file invocations like `gos run /tmp/foo.gos`.
fn is_inside_project(dir: &Path) -> bool {
    if dir.join("project.toml").is_file() {
        return true;
    }
    if let Some(parent) = dir.parent() {
        if parent.join("project.toml").is_file() {
            return true;
        }
    }
    false
}

fn is_valid_module_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
    }
    true
}

/// Renders a `std::io::Error` as a clean diagnostic free of
/// libc artefacts (`(os error N)` tails, `stat`/`reading`
/// syscall prefixes). Path-aware where a path is available.
pub(crate) fn friendly_io_error(err: std::io::Error, path: &Path) -> anyhow::Error {
    use std::io::ErrorKind;
    let display = path.display();
    let msg = match err.kind() {
        ErrorKind::NotFound => format!("file not found: {display}"),
        ErrorKind::PermissionDenied => format!("permission denied: {display}"),
        ErrorKind::IsADirectory => format!("expected a file, found a directory: {display}"),
        ErrorKind::NotADirectory => format!("expected a directory, found a file: {display}"),
        ErrorKind::AlreadyExists => format!("already exists: {display}"),
        ErrorKind::InvalidData => format!("invalid file contents: {display}"),
        ErrorKind::TimedOut => format!("timed out reading {display}"),
        ErrorKind::WriteZero => format!("could not write to {display}"),
        // Other (genuinely surprising) errors keep the kind name
        // so the user has something to grep for, but still drop
        // the libc `(os error N)` tail that std prepends.
        kind => format!("{display}: {kind:?}"),
    };
    anyhow!(msg)
}

/// Resolves a source argument while preserving explicit user intent.
/// Existing files are read exactly as named, regardless of extension.
/// A missing extensionless path may still resolve to `path.gos` as a
/// convenience for `gos run foo`.
pub(crate) fn resolve_gos_source(path: &Path) -> PathBuf {
    if path.exists() {
        return path.to_path_buf();
    }
    if path.extension().is_none() {
        let with_ext = path.with_extension("gos");
        if with_ext.exists() {
            return with_ext;
        }
    }
    path.to_path_buf()
}

/// Current-project filesystem context discovered from the process cwd.
///
/// Commands often need the nearest `project.toml` more than once during a
/// single invocation (runner dispatch, source entry resolution, static binding
/// setup). This memoizes only the manifest lookup and parse; source files and
/// dependency bundles still read fresh from disk at their existing call sites.
#[derive(Debug, Clone)]
pub(crate) struct ProjectContext {
    pub(crate) cwd: PathBuf,
    pub(crate) manifest_path: Option<PathBuf>,
    manifest: Option<std::result::Result<Manifest, String>>,
}

impl ProjectContext {
    pub(crate) fn manifest_dir(&self) -> Option<PathBuf> {
        self.manifest_path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    }

    pub(crate) fn manifest_result(&self) -> Option<std::result::Result<&Manifest, &str>> {
        self.manifest
            .as_ref()
            .map(|result| result.as_ref().map_err(String::as_str))
    }
}

/// Returns the memoized current-project context, recomputing when cwd changes.
pub(crate) fn project_context() -> ProjectContext {
    static CACHE: Mutex<Option<ProjectContext>> = Mutex::new(None);

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut guard = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(ctx) = guard.as_ref()
        && ctx.cwd == cwd
    {
        return ctx.clone();
    }

    let manifest_path = find_manifest(&cwd);
    let manifest = manifest_path.as_ref().and_then(|path| {
        let text = fs::read_to_string(path).ok()?;
        Some(Manifest::parse(&text).map_err(|err| format!("{}: {err}", path.display())))
    });
    let ctx = ProjectContext {
        cwd,
        manifest_path,
        manifest,
    };
    *guard = Some(ctx.clone());
    ctx
}

/// The current project's source edition, falling back to the compatibility
/// edition for loose-file invocations and malformed or absent manifests.
#[must_use]
pub(crate) fn project_edition() -> gossamer_pkg::Edition {
    project_context()
        .manifest_result()
        .and_then(Result::ok)
        .map_or(gossamer_pkg::Edition::E2026, |manifest| {
            manifest.project.edition
        })
}

/// The source edition for the project containing `entry`, falling back to the
/// compatibility edition for loose-file invocations and malformed manifests.
#[must_use]
pub(crate) fn project_edition_for_entry(entry: &Path) -> gossamer_pkg::Edition {
    let Some(root) = project_root_for_entry(entry) else {
        return gossamer_pkg::Edition::E2026;
    };
    let path = root.join("project.toml");
    fs::read_to_string(&path)
        .ok()
        .and_then(|text| Manifest::parse(&text).ok())
        .map_or(gossamer_pkg::Edition::E2026, |manifest| {
            manifest.project.edition
        })
}

/// Walks up from the cwd looking for the nearest `project.toml`.
/// Returns the directory that contains it (the project root).
pub(crate) fn find_project_root() -> Option<PathBuf> {
    project_context().manifest_dir()
}

/// Default source root for whole-project commands (`check`, `lint`,
/// `test`): the project root's `src/` if present, otherwise the
/// project root itself, otherwise the current directory.
pub(crate) fn default_test_root() -> Result<PathBuf> {
    if let Some(root) = find_project_root() {
        let src = root.join("src");
        if src.is_dir() {
            return Ok(src);
        }
        return Ok(root);
    }
    std::env::current_dir().context("read current directory")
}

/// Resolves an entry-point command argument to a concrete `.gos` file.
/// `None` uses the nearest project's conventional entry; a directory
/// argument resolves that directory's project entry (so `gos
/// my_project` works); a file argument is used as given.
pub(crate) fn resolve_entry_arg(path: Option<PathBuf>) -> Result<PathBuf> {
    match path {
        None => default_main_entry(),
        Some(p) if p.is_dir() => resolve_project_entry(&p),
        Some(p) => Ok(p),
    }
}

/// Returns the manifest root containing `entry`, when it belongs to a project.
/// Used by the development supervisor to watch the complete local source tree.
pub(crate) fn project_root_for_entry(entry: &Path) -> Option<PathBuf> {
    entry.parent().and_then(|dir| {
        dir.ancestors()
            .find(|candidate| candidate.join("project.toml").is_file())
            .map(Path::to_path_buf)
    })
}

/// Discovers every transitive local `path` dependency of `entry`'s project.
/// This shares the traversal used by [`read_entry_source`] so the development
/// watcher observes exactly the source trees included in a bundled revision.
pub(crate) fn local_path_dependency_roots(entry: &Path) -> Vec<PathBuf> {
    let mut visited = Vec::new();
    let mut worklist = Vec::new();
    collect_path_deps(entry, &mut visited, &mut worklist);
    while let Some((root, dep_entry)) = worklist.pop() {
        collect_path_deps(&dep_entry, &mut visited, &mut worklist);
        if !visited.contains(&root) {
            visited.push(root);
        }
    }
    visited
}

/// Default entry point for whole-project run/build commands.
/// Resolves via [`resolve_project_entry`] from the nearest project
/// root; returns `Err` with a useful diagnostic otherwise.
pub(crate) fn default_main_entry() -> Result<PathBuf> {
    let root = find_project_root().ok_or_else(|| {
        anyhow!(
            "no project.toml found above the current directory; pass a path or run from inside a project"
        )
    })?;
    resolve_project_entry(&root)
}

/// Entry-point resolution for a project root. An explicit `[project] entry`
/// in the manifest wins; otherwise the convention order applies:
/// `src/main.gos`, `main.gos`, the manifest-id-named source
/// (`src/<id-tail>.gos`, then `<id-tail>.gos`), and finally a sole
/// `.gos` candidate under `src/` or the root. A directory with
/// several nameless candidates is an error that lists them.
pub(crate) fn resolve_project_entry(root: &Path) -> Result<PathBuf> {
    if let Some(entry) = manifest_entry(root) {
        let path = root.join(&entry);
        if path.is_file() {
            return Ok(path);
        }
        return Err(anyhow!(
            "project.toml sets [project] entry = {:?} but {} does not exist",
            entry,
            path.display()
        ));
    }
    let canonical = root.join("src").join("main.gos");
    if canonical.is_file() {
        return Ok(canonical);
    }
    let bare = root.join("main.gos");
    if bare.is_file() {
        return Ok(bare);
    }
    if let Some(tail) = manifest_id_tail(root) {
        let named = root.join("src").join(format!("{tail}.gos"));
        if named.is_file() {
            return Ok(named);
        }
        let named = root.join(format!("{tail}.gos"));
        if named.is_file() {
            return Ok(named);
        }
    }
    for dir in [root.join("src"), root.to_path_buf()] {
        match entry_candidates(&dir).as_slice() {
            [] => {}
            [sole] => return Ok(sole.clone()),
            many => {
                let names: Vec<String> = many
                    .iter()
                    .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .collect();
                return Err(anyhow!(
                    "project root {} has no src/main.gos (or main.gos), and {} holds several candidates ({}); pass a path explicitly",
                    root.display(),
                    dir.display(),
                    names.join(", ")
                ));
            }
        }
    }
    Err(anyhow!(
        "project root {} has no src/main.gos (or main.gos) and no .gos source to run; pass a path explicitly",
        root.display()
    ))
}

/// Last segment of the manifest's `[project] id`, when the root's
/// `project.toml` parses.
fn manifest_id_tail(root: &Path) -> Option<String> {
    let text = fs::read_to_string(root.join("project.toml")).ok()?;
    let manifest = gossamer_pkg::Manifest::parse(&text).ok()?;
    Some(manifest.project.id.tail().to_string())
}

/// `[project] entry` from the root's manifest, when present and the
/// `project.toml` parses.
fn manifest_entry(root: &Path) -> Option<String> {
    let text = fs::read_to_string(root.join("project.toml")).ok()?;
    gossamer_pkg::Manifest::parse(&text).ok()?.project.entry
}

/// `.gos` files in `dir` that qualify as an entry point, sorted by
/// name. Skips `_`-prefixed scratch files and `*_test.gos` (the
/// same exclusions the sibling auto-bundler applies).
fn entry_candidates(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = Vec::new();
    for dirent in read.flatten() {
        let path = dirent.path();
        if !path.is_file() || path.extension().and_then(|s| s.to_str()) != Some("gos") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem.starts_with('_') || stem.ends_with("_test") {
            continue;
        }
        out.push(path);
    }
    out.sort();
    out
}

/// Recursively gathers every `.gos` file under `root`. If `root`
/// names a single file, returns it as a one-element list.
pub(crate) fn collect_lint_targets(root: &PathBuf) -> Result<Vec<PathBuf>> {
    let meta = fs::metadata(root).map_err(|e| friendly_io_error(e, root))?;
    if meta.is_file() {
        return Ok(vec![root.clone()]);
    }
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("read_dir {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|s| s.to_str()) == Some("gos") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Picks the default binary name for a build, matching Rust's
/// `cargo build` rule: derive from the package name, not the
/// source filename. When the source sits inside a project (a
/// `project.toml` is found by walking parents), the binary takes
/// the last `/`-separated segment of `[project] id`. Loose-file
/// builds with no manifest fall back to the source stem.
pub(crate) fn default_unit_name(file: &Path) -> String {
    if let Some(manifest_path) = gossamer_pkg::find_manifest(file)
        && let Ok(text) = fs::read_to_string(&manifest_path)
        && let Ok(manifest) = gossamer_pkg::Manifest::parse(&text)
    {
        return manifest.project.id.tail().to_string();
    }
    file.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("main")
        .to_string()
}

/// Resolves the build output path.
///
/// Resolution order (first hit wins):
/// 1. `project.output` in the nearest enclosing `project.toml`
///    (relative paths resolve against the manifest's directory).
/// 2. `<project-root>/target/{debug,release}/<unit>`.
/// 3. `<source-dir>/target/{debug,release}/<unit>` for loose-file
///    builds with no manifest.
///
/// `target_is_windows` names the *produced binary's* OS, which is the
/// host's only for a native build - a `--target` cross build is always
/// Linux (the only OS `gos build` can cross-produce), regardless of which
/// OS the compiler itself runs on. Using `cfg!(windows)` (the host) here
/// unconditionally would misname a Linux binary cross-built from a
/// Windows host with a trailing `.exe`.
pub(crate) fn resolve_output_path(
    file: &Path,
    unit_name: &str,
    release: bool,
    target_is_windows: bool,
) -> Result<PathBuf> {
    if let Some(manifest_path) = gossamer_pkg::find_manifest(file) {
        let manifest_text =
            fs::read_to_string(&manifest_path).map_err(|e| friendly_io_error(e, &manifest_path))?;
        let manifest = gossamer_pkg::Manifest::parse(&manifest_text)
            .with_context(|| format!("parsing {}", manifest_path.display()))?;
        if let Some(output) = manifest.project.output {
            let mut raw = PathBuf::from(&output);
            // A manifest `output` with no extension still needs the
            // platform executable suffix on Windows, or the linker
            // writes a non-runnable extensionless file. An explicit
            // extension (`tool.exe`, `tool.bin`) is left untouched.
            if target_is_windows && raw.extension().is_none() {
                raw.set_extension("exe");
            }
            let resolved = if raw.is_absolute() {
                raw
            } else {
                manifest_path
                    .parent()
                    .map_or_else(|| raw.clone(), |dir| dir.join(&raw))
            };
            return Ok(resolved);
        }
        if let Some(root) = manifest_path.parent() {
            let profile = if release { "release" } else { "debug" };
            let target_dir = root.join("target").join(profile);
            fs::create_dir_all(&target_dir)
                .map_err(|e| anyhow!("creating {}: {e}", target_dir.display()))?;
            return Ok(target_dir.join(platform_exe_name(unit_name, target_is_windows)));
        }
    }
    let parent = file.parent().filter(|p| !p.as_os_str().is_empty());
    let base = parent.map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let profile = if release { "release" } else { "debug" };
    let target_dir = base.join("target").join(profile);
    fs::create_dir_all(&target_dir)
        .map_err(|e| anyhow!("creating {}: {e}", target_dir.display()))?;
    Ok(target_dir.join(platform_exe_name(unit_name, target_is_windows)))
}

/// Binary name with the correct platform extension: `stem.exe` when the
/// *produced binary's* target OS is Windows, bare `stem` otherwise. See
/// [`resolve_output_path`] for why this is not simply `cfg!(windows)`.
pub(crate) fn platform_exe_name(stem: &str, target_is_windows: bool) -> String {
    if target_is_windows {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

/// Returns the path the REPL uses to persist line-edit history
/// across sessions. Prefers `$GOSSAMER_HISTORY` → `$XDG_STATE_HOME/
/// gossamer/history` → `$HOME/.gossamer_history`. `None` is returned
/// only when no reasonable home directory can be discovered, in
/// which case history is kept in-memory for the current session.
pub(crate) fn repl_history_path() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("GOSSAMER_HISTORY") {
        return Some(PathBuf::from(explicit));
    }
    if let Ok(state) = std::env::var("XDG_STATE_HOME") {
        let mut path = PathBuf::from(state);
        path.push("gossamer");
        let _ = fs::create_dir_all(&path);
        path.push("history");
        return Some(path);
    }
    if let Ok(home) = std::env::var("HOME") {
        let mut path = PathBuf::from(home);
        path.push(".gossamer_history");
        return Some(path);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_project(tag: &str, manifest: &str, files: &[&str]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("gos-entry-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("project.toml"), manifest).unwrap();
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "fn main() { }\n").unwrap();
        }
        root
    }

    const MANIFEST: &str = "[project]\nid = \"example.com/widget\"\nversion = \"0.1.0\"\n";

    #[test]
    fn entry_prefers_src_main() {
        let root = scratch_project("srcmain", MANIFEST, &["src/main.gos", "widget.gos"]);
        assert_eq!(
            resolve_project_entry(&root).unwrap(),
            root.join("src").join("main.gos")
        );
    }

    #[test]
    fn entry_falls_back_to_manifest_id_name() {
        let root = scratch_project("idname", MANIFEST, &["widget.gos", "helper.gos"]);
        assert_eq!(
            resolve_project_entry(&root).unwrap(),
            root.join("widget.gos")
        );
    }

    #[test]
    fn entry_uses_sole_candidate() {
        let root = scratch_project(
            "sole",
            MANIFEST,
            &["tool.gos", "_scratch.gos", "x_test.gos"],
        );
        assert_eq!(resolve_project_entry(&root).unwrap(), root.join("tool.gos"));
    }

    #[test]
    fn entry_edition_comes_from_entry_project_not_cwd() {
        let root = scratch_project(
            "entryedition",
            "[project]\nid = \"example.com/lazy\"\nversion = \"0.1.0\"\nedition = \"2027\"\n",
            &["main.gos"],
        );
        assert_eq!(
            project_edition_for_entry(&root.join("main.gos")),
            gossamer_pkg::Edition::E2027
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn entry_ambiguity_lists_candidates() {
        let root = scratch_project("ambig", MANIFEST, &["alpha.gos", "beta.gos"]);
        let err = resolve_project_entry(&root).unwrap_err().to_string();
        assert!(err.contains("alpha.gos"), "missing candidate in: {err}");
        assert!(err.contains("beta.gos"), "missing candidate in: {err}");
    }

    #[test]
    fn entry_errors_when_no_sources() {
        let root = scratch_project("empty", MANIFEST, &[]);
        let err = resolve_project_entry(&root).unwrap_err().to_string();
        assert!(err.contains("no .gos source"), "unexpected: {err}");
    }

    #[test]
    fn source_resolution_preserves_existing_explicit_paths() {
        let dir = std::env::temp_dir().join(format!("gos-source-path-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let extensionless = dir.join("tool");
        let explicit = dir.join("tool.py");
        let inferred = dir.join("tool.gos");
        for path in [&extensionless, &explicit, &inferred] {
            fs::write(path, "fn main() {}\n").unwrap();
        }

        assert_eq!(resolve_gos_source(&extensionless), extensionless);
        assert_eq!(resolve_gos_source(&explicit), explicit);
        let missing_extensionless = dir.join("missing");
        let missing_gos = dir.join("missing.gos");
        fs::write(&missing_gos, "fn main() {}\n").unwrap();
        assert_eq!(resolve_gos_source(&missing_extensionless), missing_gos);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundle_includes_siblings_and_subdirectory_modules() {
        let root = std::env::temp_dir().join(format!("gos-bundle-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src").join("sub").join("deep")).unwrap();
        fs::write(root.join("project.toml"), MANIFEST).unwrap();
        let entry = root.join("src").join("main.gos");
        fs::write(&entry, "mod helper;\nmod sub;\nfn main() { }\n").unwrap();
        fs::write(root.join("src").join("helper.gos"), "pub fn h() { }\n").unwrap();
        fs::write(
            root.join("src").join("sub").join("mod.gos"),
            "pub fn ping() { }\n",
        )
        .unwrap();
        fs::write(
            root.join("src").join("sub").join("deep").join("mod.gos"),
            "pub fn depth() { }\n",
        )
        .unwrap();

        let bundled = bundle_sibling_modules(&entry, fs::read_to_string(&entry).unwrap());
        // Flat sibling -> top-level module.
        assert!(
            bundled.contains("mod helper {"),
            "no helper module:\n{bundled}"
        );
        assert!(bundled.contains("pub fn h"), "no helper body:\n{bundled}");
        // Subdirectory with mod.gos -> module, recursively including its
        // own subdirectory module.
        assert!(bundled.contains("mod sub {"), "no sub module:\n{bundled}");
        assert!(bundled.contains("pub fn ping"), "no sub body:\n{bundled}");
        assert!(
            bundled.contains("mod deep {"),
            "no nested deep module:\n{bundled}"
        );
        assert!(bundled.contains("pub fn depth"), "no deep body:\n{bundled}");
        // The entry's `mod NAME;` declarations are neutralized so the
        // synthetic inline bodies are the sole definitions.
        assert!(
            bundled.contains("(sibling auto-bundled)"),
            "mod decls not neutralized:\n{bundled}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn loose_file_outside_project_is_not_bundled() {
        let dir = std::env::temp_dir().join(format!("gos-loose-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let entry = dir.join("a.gos");
        fs::write(&entry, "fn main() { }\n").unwrap();
        fs::write(dir.join("b.gos"), "pub fn other() { }\n").unwrap();
        // No project.toml -> a loose-file invocation must not pull in
        // unrelated siblings.
        let bundled = bundle_sibling_modules(&entry, "fn main() { }\n".to_string());
        assert!(
            !bundled.contains("mod b"),
            "loose file wrongly bundled:\n{bundled}"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
