//! Package-management subcommands: `add`, `remove`, `tidy`,
//! `fetch`, `vendor`. Each operates on the nearest enclosing
//! `project.toml` (or an explicit `--manifest PATH`).

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};

use crate::paths::friendly_io_error;

/// `gos add SPEC [--manifest PATH]` — declares a registry
/// dependency. `SPEC` is `<id>` or `<id>@<version>`.
pub(crate) fn add(spec: &str, manifest: Option<PathBuf>) -> Result<()> {
    let path = manifest.unwrap_or_else(|| PathBuf::from("project.toml"));
    let (id_text, version_text) = match spec.split_once('@') {
        Some((id, ver)) => (id, ver),
        None => (spec, "0.1.0"),
    };
    let id = gossamer_pkg::ProjectId::parse(id_text)
        .with_context(|| format!("invalid id `{id_text}`"))?;
    let version = gossamer_pkg::Version::parse(version_text)
        .with_context(|| format!("invalid version `{version_text}`"))?;
    let source = fs::read_to_string(&path).map_err(|e| friendly_io_error(e, &path))?;
    let mut m = gossamer_pkg::Manifest::parse(&source)?;
    let changed = gossamer_pkg::add_registry(&mut m, &id, version);
    fs::write(&path, m.render()).with_context(|| format!("writing {}", path.display()))?;
    println!(
        "add: {action} {id} ({version})",
        action = if changed { "added" } else { "kept" }
    );
    Ok(())
}

/// `gos add --rust-binding SPEC` — declares an entry in
/// `[rust-bindings]`. Three spec shapes are supported:
///
/// - `<crate>` — crates.io with version `0.0.1` placeholder
///   (user is expected to update it).
/// - `<crate>@<version>` — crates.io with explicit version.
/// - `path:<dir>` — local Cargo crate at `<dir>` (interpreted
///   relative to the manifest).
///
/// For crates that don't already depend on `gossamer-binding`,
/// scaffolds a wrapper crate under `.gos-bindings/<name>/` and
/// rewrites the manifest entry to point at the wrapper.
pub(crate) fn add_rust_binding(spec: &str, manifest: Option<PathBuf>) -> Result<()> {
    let path = manifest.unwrap_or_else(|| PathBuf::from("project.toml"));
    let parent = path
        .parent()
        .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf);
    let source = fs::read_to_string(&path).map_err(|e| friendly_io_error(e, &path))?;
    let mut m = gossamer_pkg::Manifest::parse(&source)?;

    let (name, binding) = parse_rust_binding_spec(spec, &parent)?;
    if !is_valid_cargo_name(&name) {
        return Err(anyhow!("invalid crate name `{name}`"));
    }

    let action = if m.rust_bindings.contains_key(&name) {
        "kept"
    } else {
        "added"
    };
    m.rust_bindings.insert(name.clone(), binding.clone());
    fs::write(&path, m.render()).with_context(|| format!("writing {}", path.display()))?;

    let scaffolded = scaffold_wrapper_if_needed(&name, &binding, &parent)?;

    println!("add: {action} rust-binding `{name}`");
    if let Some(wrapper) = scaffolded {
        println!(
            "scaffolded wrapper at {}",
            wrapper.strip_prefix(&parent).unwrap_or(&wrapper).display()
        );
    }
    Ok(())
}

fn parse_rust_binding_spec(
    spec: &str,
    manifest_dir: &std::path::Path,
) -> Result<(String, gossamer_pkg::RustBindingSpec)> {
    if let Some(rest) = spec.strip_prefix("path:") {
        let abs = if std::path::Path::new(rest).is_absolute() {
            PathBuf::from(rest)
        } else {
            manifest_dir.join(rest)
        };
        let crate_name = read_cargo_package_name(&abs)
            .with_context(|| format!("reading {}/Cargo.toml", abs.display()))?;
        let binding = gossamer_pkg::RustBindingSpec::Path {
            version: None,
            path: rest.to_string(),
            features: Vec::new(),
            default_features: true,
        };
        return Ok((crate_name, binding));
    }
    let (name, version) = match spec.split_once('@') {
        Some((n, v)) => (n.to_string(), Some(v.to_string())),
        None => (spec.to_string(), None),
    };
    let version_text = version.unwrap_or_else(|| "0.0.1".to_string());
    let normalized = normalize_version(&version_text);
    let range = gossamer_pkg::CaretRange::parse(&normalized)
        .with_context(|| format!("parsing version `{version_text}`"))?;
    let binding = gossamer_pkg::RustBindingSpec::Crates {
        version: range,
        features: Vec::new(),
        default_features: true,
    };
    Ok((name, binding))
}

fn normalize_version(input: &str) -> String {
    let stripped = input.trim().trim_start_matches('^');
    let parts: Vec<&str> = stripped.split('.').collect();
    match parts.len() {
        1 => format!("{}.0.0", parts[0]),
        2 => format!("{}.{}.0", parts[0], parts[1]),
        _ => stripped.to_string(),
    }
}

fn read_cargo_package_name(crate_root: &std::path::Path) -> Result<String> {
    let cargo_toml = crate_root.join("Cargo.toml");
    let text = fs::read_to_string(&cargo_toml)?;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("name") {
            let after_eq = rest.trim().strip_prefix('=').map(str::trim);
            if let Some(value) = after_eq
                && let Some(stripped) = value.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
            {
                return Ok(stripped.to_string());
            }
        }
    }
    Err(anyhow!(
        "Cargo.toml at {} is missing a `name = \"...\"` line",
        cargo_toml.display()
    ))
}

fn is_valid_cargo_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn scaffold_wrapper_if_needed(
    name: &str,
    binding: &gossamer_pkg::RustBindingSpec,
    manifest_dir: &std::path::Path,
) -> Result<Option<PathBuf>> {
    let crate_root = match binding {
        gossamer_pkg::RustBindingSpec::Path { path, .. } => {
            if std::path::Path::new(path).is_absolute() {
                PathBuf::from(path)
            } else {
                manifest_dir.join(path)
            }
        }
        _ => return Ok(None),
    };
    let cargo_toml = crate_root.join("Cargo.toml");
    if !cargo_toml.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&cargo_toml)?;
    if text.contains("gossamer-binding") {
        return Ok(None);
    }
    let wrapper_dir = manifest_dir
        .join(".gos-bindings")
        .join(format!("gos-{name}"));
    if wrapper_dir.exists() {
        return Ok(Some(wrapper_dir));
    }
    fs::create_dir_all(wrapper_dir.join("src"))?;
    let dep_abs = if crate_root.is_absolute() {
        crate_root.clone()
    } else {
        std::fs::canonicalize(&crate_root).unwrap_or_else(|_| crate_root.clone())
    };
    let wrapper_cargo_toml = format!(
        "[package]\nname = \"gos-{name}\"\nversion = \"0.0.1\"\nedition = \"2024\"\npublish = false\n\n[workspace]\n\n[lib]\ncrate-type = [\"rlib\"]\n\n[dependencies]\n{name} = {{ path = \"{}\" }}\ngossamer-binding = {{ path = \"{}\" }}\n",
        dep_abs.display(),
        crate::binding_dispatch::locate_gossamer_root().map_or_else(
            || "../../../crates/gossamer-binding".to_string(),
            |r| r
                .join("crates")
                .join("gossamer-binding")
                .display()
                .to_string(),
        )
    );
    fs::write(wrapper_dir.join("Cargo.toml"), wrapper_cargo_toml)?;
    let symbol_prefix = name.replace('-', "_");
    let wrapper_lib = format!(
        "//! Wrapper crate exposing `{name}` to Gossamer code.\n//!\n//! Fill in the `register_module!` block(s) below to expose\n//! the API surface you need from `{name}`.\n\nuse gossamer_binding::register_module;\n\nregister_module!(\n    binding,\n    path: \"{symbol_prefix}\",\n    symbol_prefix: {symbol_prefix},\n    doc: \"Bindings for the `{name}` Rust crate.\",\n\n    // Example:\n    // fn version() -> String {{\n    //     env!(\"CARGO_PKG_VERSION\").to_string()\n    // }}\n);\n\n/// Linker-hook: must be called from the runner template so the\n/// linkme entries survive LTO.\npub fn __bindings_force_link() {{\n    binding::force_link();\n}}\n",
    );
    fs::write(wrapper_dir.join("src").join("lib.rs"), wrapper_lib)?;
    Ok(Some(wrapper_dir))
}

/// `gos remove ID [--manifest PATH]` — drops the matching
/// dependency entry; errors when nothing matched.
pub(crate) fn remove(id_text: &str, manifest: Option<PathBuf>) -> Result<()> {
    let path = manifest.unwrap_or_else(|| PathBuf::from("project.toml"));
    let id = gossamer_pkg::ProjectId::parse(id_text)
        .with_context(|| format!("invalid id `{id_text}`"))?;
    let source = fs::read_to_string(&path).map_err(|e| friendly_io_error(e, &path))?;
    let mut m = gossamer_pkg::Manifest::parse(&source)?;
    let removed = gossamer_pkg::remove(&mut m, &id);
    if !removed {
        return Err(anyhow!("dependency {id} is not declared"));
    }
    fs::write(&path, m.render()).with_context(|| format!("writing {}", path.display()))?;
    println!("remove: dropped {id}");
    Ok(())
}

/// `gos tidy [--manifest PATH]` — re-renders the manifest so
/// whitespace + entry ordering match the canonical layout.
pub(crate) fn tidy(manifest: Option<PathBuf>) -> Result<()> {
    let path = manifest.unwrap_or_else(|| PathBuf::from("project.toml"));
    let source = fs::read_to_string(&path).map_err(|e| friendly_io_error(e, &path))?;
    let m = gossamer_pkg::Manifest::parse(&source)?;
    fs::write(&path, m.render()).with_context(|| format!("writing {}", path.display()))?;
    println!("tidy: canonicalised {}", path.display());
    Ok(())
}

/// `gos fetch [--manifest PATH] [--offline]` — populates the
/// download cache for every transitive dependency.
pub(crate) fn fetch(manifest: Option<PathBuf>, offline: bool) -> Result<()> {
    let path = manifest.unwrap_or_else(|| PathBuf::from("project.toml"));
    let source = fs::read_to_string(&path).map_err(|e| friendly_io_error(e, &path))?;
    let m = gossamer_pkg::Manifest::parse(&source)?;
    let resolver = gossamer_pkg::Resolver::new(gossamer_pkg::VersionCatalogue::new());
    let resolved = resolver
        .resolve(&m)
        .map_err(|e| anyhow!("resolve failed: {e}"))?;
    let mut cache = gossamer_pkg::Cache::new();
    let fetcher = gossamer_pkg::Fetcher::new(gossamer_pkg::FetchOptions { offline });
    let fetched = fetcher
        .fetch_all(&resolved, &mut cache)
        .map_err(|e| anyhow!("fetch failed: {e}"))?;
    println!("fetch: {} project(s) cached", fetched.len());
    for entry in &fetched {
        println!("  {} → {}", entry.resolved.id, entry.source.digest);
    }
    Ok(())
}

/// `gos vendor [--manifest PATH] [--out DIR]` — materialises every
/// transitive dependency into `<out>/` for an offline / reproducible
/// build.
pub(crate) fn vendor(manifest: Option<PathBuf>, out: Option<PathBuf>) -> Result<()> {
    let path = manifest.unwrap_or_else(|| PathBuf::from("project.toml"));
    let source = fs::read_to_string(&path).map_err(|e| friendly_io_error(e, &path))?;
    let m = gossamer_pkg::Manifest::parse(&source)?;
    let resolver = gossamer_pkg::Resolver::new(gossamer_pkg::VersionCatalogue::new());
    let resolved = resolver
        .resolve(&m)
        .map_err(|e| anyhow!("resolve failed: {e}"))?;
    let mut cache = gossamer_pkg::Cache::new();
    let fetcher = gossamer_pkg::Fetcher::new(gossamer_pkg::FetchOptions::default());
    let fetched = fetcher
        .fetch_all(&resolved, &mut cache)
        .map_err(|e| anyhow!("fetch failed: {e}"))?;
    let dest = out.unwrap_or_else(|| PathBuf::from("vendor"));
    let written = gossamer_pkg::vendor(&fetched, &dest)
        .with_context(|| format!("writing vendor dir {}", dest.display()))?;
    let total: usize = written.values().map(Vec::len).sum();
    println!(
        "vendor: wrote {total} file(s) for {} project(s) to {}",
        written.len(),
        dest.display()
    );
    Ok(())
}
