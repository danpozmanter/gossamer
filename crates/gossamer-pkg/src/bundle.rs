//! Assembles a project's compilation unit from its on-disk layout.
//!
//! An entry file's siblings and subdirectory `mod.gos` packages become
//! inline `mod NAME { ... }` items appended to the entry source, and
//! every `path = "..."` dependency is inlined the same way. Every
//! front end that type-checks project code - the CLI and the language
//! server alike - assembles the same unit, so a cross-module reference
//! resolves identically in an editor and on the command line.
//!
//! Appending (never prefixing) keeps every byte offset in the entry
//! source unchanged, so diagnostics map back to the file the user is
//! editing.

use std::fs;
use std::path::{Path, PathBuf};

use crate::{DependencySpec, InlineDependency, Manifest};

/// Inlines every `path = "..."` dependency of the entry's project as
/// a top-level `mod <dep-module-name> { <dep source> }`. Transitive
/// path dependencies hoist to top-level modules too (deduplicated by
/// canonical root), so a dependency's own `use "id" as alias` binds
/// against a sibling module exactly as the consumer's does. The
/// module name derives from the dependency's project id via
/// `gossamer_resolve::project_dep_module_name`, which is also what
/// the resolver binds `use "id" as alias` against. Non-path
/// dependencies are untouched.
#[must_use]
pub fn bundle_path_dependencies(
    entry: &Path,
    source: String,
    visited: &mut Vec<PathBuf>,
) -> String {
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
pub fn collect_path_deps(
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
    let Ok(manifest) = Manifest::parse(&manifest_text) else {
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
fn dependency_path(spec: &DependencySpec) -> Option<&str> {
    match spec {
        DependencySpec::Inline(InlineDependency::Path { path }) => Some(path),
        _ => None,
    }
}

/// Resolves a path dependency's project id and entry source file:
/// `lib.gos` (flat or under `src/`) is the library entry; `main.gos`
/// is accepted as a fallback for binary-shaped projects.
fn path_dep_entry(dep_root: &Path) -> Option<(String, PathBuf)> {
    let manifest_text = fs::read_to_string(dep_root.join("project.toml")).ok()?;
    let manifest = Manifest::parse(&manifest_text).ok()?;
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

/// One auto-bundled module: its `name`, its already-assembled `body`,
/// and the source path it came from (for the bundle comment).
struct BundledModule {
    name: String,
    body: String,
    origin: PathBuf,
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
#[must_use]
pub fn bundle_sibling_modules(entry: &Path, source: String) -> String {
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
                    // Blank the declaration in place rather than
                    // prefixing a comment: an editor maps positions
                    // against this text, so the rewrite must not move
                    // any byte that follows it on the line.
                    for ch in line.chars() {
                        out.push(if ch == '\n' || ch == '\r' { ch } else { ' ' });
                    }
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

/// Assembles the compilation unit for `entry` from `source`: its
/// sibling / subdirectory modules first, then every `path = "..."`
/// dependency. `source` is the entry's text, which lets an editor pass
/// its unsaved buffer while the rest of the unit is read from disk.
#[must_use]
pub fn bundle_entry_source(entry: &Path, source: String) -> String {
    let bundled = bundle_sibling_modules(entry, source);
    let mut visited = Vec::new();
    bundle_path_dependencies(entry, bundled, &mut visited)
}

#[cfg(test)]
mod bundle_tests {
    use std::fs;

    use super::*;

    const MANIFEST: &str = "[project]\nid = \"example.com/widget\"\nversion = \"0.1.0\"\n";

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
        // The entry's `mod NAME;` declarations are blanked in place, so
        // the inline bodies are the sole definitions and every byte that
        // follows keeps its offset.
        assert!(
            !bundled.starts_with("mod helper;"),
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

    #[test]
    fn neutralized_mod_decl_preserves_every_following_offset() {
        let source = "mod helper;\nfn main() { }\n";
        let blanked = neutralize_external_mod_decls(source, &["helper"]);
        assert_eq!(
            blanked.len(),
            source.len(),
            "neutralization must not move any byte: {blanked:?}"
        );
        assert_eq!(
            source.find("fn main"),
            blanked.find("fn main"),
            "code after a neutralized decl must keep its offset"
        );
    }
}
