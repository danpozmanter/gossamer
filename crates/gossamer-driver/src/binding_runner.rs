//! Per-project Rust-binding runner.
//!
//! When a `project.toml` declares a non-empty `[rust-bindings]`
//! section, `gos run` / `gos build` re-execs into a *runner*
//! binary that statically links every binding's Cargo crate. The
//! runner is built on demand by Cargo and cached under
//! `$XDG_CACHE_HOME/gossamer/runners/<fp>` keyed by the manifest's
//! [`Manifest::rust_binding_fingerprint`].
//!
//! Three artefacts can be materialised under the same workdir:
//!
//! - `runner/` — the executable runner used by `gos run`.
//! - `staticlib/` — `libgos_static_bindings.a` used by the
//!   compiled-mode link step.
//! - `sigs/signatures.json` — JSON dump of every binding's module
//!   + item signature, fed to the resolver / typechecker.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::SystemTime;

use gossamer_pkg::{GitRef, Manifest, RustBindingSpec};
use gossamer_runner_template::{
    BindingEntry, Profile as TmplProfile, RenderInput, render_cargo_toml, render_main_rs,
    render_sigs_dump_rs, render_staticlib_cargo_toml, render_staticlib_lib_rs,
};
use thiserror::Error;

/// Cache subdirectory of the runner executable.
const SUBDIR_RUNNER: &str = "runner";
/// Cache subdirectory of the staticlib build.
const SUBDIR_STATICLIB: &str = "staticlib";
/// Cache subdirectory of the signatures dump.
const SUBDIR_SIGS: &str = "sigs";

/// Errors raised by [`BindingRunner`] / [`StaticBindingsLib`].
#[derive(Debug, Error)]
pub enum BindingRunnerError {
    /// `cargo` was not found on PATH.
    #[error("this project declares `[rust-bindings]`; install Rust + cargo from https://rustup.rs")]
    CargoMissing,
    /// `cargo build` failed for the runner / staticlib.
    #[error("cargo build failed for binding `{crate_name}`:\n{stderr}")]
    CargoFailed {
        /// Crate that failed (or "<runner>"/"<staticlib>" when the
        /// failure can't be attributed to one binding).
        crate_name: String,
        /// Captured cargo stderr, verbatim.
        stderr: String,
    },
    /// I/O error while preparing the cache.
    #[error("cache i/o error: {0}")]
    Io(#[from] io::Error),
    /// Template rendering failed (unexpected — rendering is total).
    #[error("template render failed: {0}")]
    Render(String),
    /// Signatures dump produced unparseable JSON.
    #[error("signature dump produced invalid json: {0}")]
    BadSignatureJson(String),
}

/// Profile (debug / release) for the runner build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// `cargo build` (no `--release`).
    Debug,
    /// `cargo build --release`.
    Release,
}

impl Profile {
    fn template_profile(self) -> TmplProfile {
        match self {
            Self::Debug => TmplProfile::Debug,
            Self::Release => TmplProfile::Release,
        }
    }

    /// Cargo profile dirname.
    #[must_use]
    pub fn dir(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }
}

/// Materialised binding metadata used by all three artefacts.
#[derive(Debug, Clone)]
pub struct RenderedBinding {
    /// Cargo crate name (matches the `[rust-bindings]` key).
    pub crate_name: String,
    /// Cargo dep line, e.g. `foo = { path = "/abs/path" }`.
    pub cargo_dep_line: String,
    /// Cargo features requested for this binding.
    pub features: Vec<String>,
    /// For path-deps, the resolved absolute crate root. Used for
    /// the source-tree mtime walk.
    pub local_root: Option<PathBuf>,
}

/// A per-project runner build.
#[derive(Debug)]
pub struct BindingRunner {
    /// Full SHA-256 of the manifest's binding set.
    pub fingerprint: [u8; 32],
    /// 12-char hex prefix of [`Self::fingerprint`].
    pub fingerprint_hex: String,
    /// Workdir under the cache (`<cache>/runners/<fp>/`).
    pub workdir: PathBuf,
    /// Bindings to link into the runner.
    pub bindings: Vec<RenderedBinding>,
    /// Absolute path to the gossamer source tree (for path deps in
    /// the rendered Cargo.toml).
    pub gossamer_root: PathBuf,
    /// Cargo profile to build with.
    pub profile: Profile,
    /// Project id for cosmetic comments in the rendered files.
    pub project_id: String,
}

impl BindingRunner {
    /// Constructs a runner from the manifest. Returns
    /// `Ok(None)` if `[rust-bindings]` is empty.
    ///
    /// `manifest_dir` is the directory containing `project.toml`;
    /// path-deps in the manifest resolve against it.
    /// `gossamer_root` is the absolute path of this checkout (the
    /// directory containing the workspace `Cargo.toml`).
    pub fn from_manifest(
        manifest: &Manifest,
        manifest_dir: &Path,
        gossamer_root: &Path,
        profile: Profile,
    ) -> io::Result<Option<Self>> {
        let cache = cache_root()?;
        Self::from_manifest_in(manifest, manifest_dir, gossamer_root, profile, &cache)
    }

    /// Same as [`Self::from_manifest`] but uses an explicit cache
    /// root instead of reading `GOSSAMER_CACHE` / `XDG_CACHE_HOME`.
    pub fn from_manifest_in(
        manifest: &Manifest,
        manifest_dir: &Path,
        gossamer_root: &Path,
        profile: Profile,
        cache_root: &Path,
    ) -> io::Result<Option<Self>> {
        if manifest.rust_bindings.is_empty() {
            return Ok(None);
        }
        let fingerprint = manifest.rust_binding_fingerprint(manifest_dir);
        let fingerprint_hex = hex_prefix(&fingerprint, 6);
        let workdir = cache_root.join("runners").join(&fingerprint_hex);
        fs::create_dir_all(&workdir)?;
        let bindings = render_bindings(&manifest.rust_bindings, manifest_dir);
        Ok(Some(Self {
            fingerprint,
            fingerprint_hex,
            workdir,
            bindings,
            gossamer_root: gossamer_root.to_path_buf(),
            profile,
            project_id: manifest.project.id.as_str().to_string(),
        }))
    }

    /// Returns the path where the runner binary will live after
    /// `ensure_built`.
    #[must_use]
    pub fn runner_binary_path(&self) -> PathBuf {
        self.workdir
            .join(SUBDIR_RUNNER)
            .join("target")
            .join(self.profile.dir())
            .join(if cfg!(windows) {
                "gos-runner.exe"
            } else {
                "gos-runner"
            })
    }

    /// Idempotently builds the runner. Returns the path to the
    /// produced binary.
    pub fn ensure_built(&self) -> Result<PathBuf, BindingRunnerError> {
        let dir = self.workdir.join(SUBDIR_RUNNER);
        fs::create_dir_all(&dir)?;
        let _lock = AdvisoryLock::acquire(&dir.join(".gos-build.lock"))?;

        let cargo_toml = dir.join("Cargo.toml");
        let main_rs = dir.join("main.rs");
        let sigs_rs = dir.join("sigs_dump.rs");

        let input = self.render_input(self.profile.template_profile());
        write_if_different(&cargo_toml, &render_cargo_toml(&input))?;
        write_if_different(&main_rs, &render_main_rs(&input))?;
        write_if_different(&sigs_rs, &render_sigs_dump_rs(&input))?;

        let bin_path = self.runner_binary_path();
        let stamp = dir.join("stamp.json");
        if self.is_fresh(&bin_path, &stamp, "runner")? {
            return Ok(bin_path);
        }
        run_cargo_build(
            &cargo_toml,
            &dir.join("target"),
            self.profile,
            "--bin",
            "gos-runner",
            "<runner>",
        )?;
        write_stamp(&stamp, &self.fingerprint_hex, self.profile, "runner")?;
        Ok(bin_path)
    }

    /// Idempotently builds the signatures bin and runs it,
    /// returning the path to `signatures.json`.
    pub fn ensure_signatures(&self) -> Result<PathBuf, BindingRunnerError> {
        // Reuse the runner's Cargo.toml — the sigs-dump bin lives
        // alongside the runner bin in the same crate.
        let dir = self.workdir.join(SUBDIR_RUNNER);
        fs::create_dir_all(&dir)?;
        let _lock = AdvisoryLock::acquire(&dir.join(".gos-build.lock"))?;

        let cargo_toml = dir.join("Cargo.toml");
        let main_rs = dir.join("main.rs");
        let sigs_rs = dir.join("sigs_dump.rs");

        let input = self.render_input(self.profile.template_profile());
        write_if_different(&cargo_toml, &render_cargo_toml(&input))?;
        write_if_different(&main_rs, &render_main_rs(&input))?;
        write_if_different(&sigs_rs, &render_sigs_dump_rs(&input))?;

        let bin_path = dir
            .join("target")
            .join(self.profile.dir())
            .join(if cfg!(windows) {
                "gos-sigs-dump.exe"
            } else {
                "gos-sigs-dump"
            });
        let sigs_dir = self.workdir.join(SUBDIR_SIGS);
        fs::create_dir_all(&sigs_dir)?;
        let json_path = sigs_dir.join("signatures.json");
        let stamp = sigs_dir.join("stamp.json");
        if self.is_fresh(&json_path, &stamp, "sigs")? && bin_path.exists() {
            return Ok(json_path);
        }
        run_cargo_build(
            &cargo_toml,
            &dir.join("target"),
            self.profile,
            "--bin",
            "gos-sigs-dump",
            "<sigs>",
        )?;
        let mut out = Command::new(&bin_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut buf = String::new();
        if let Some(mut s) = out.stdout.take() {
            s.read_to_string(&mut buf)?;
        }
        let status = out.wait()?;
        if !status.success() {
            let mut err = String::new();
            if let Some(mut s) = out.stderr.take() {
                let _ = s.read_to_string(&mut err);
            }
            return Err(BindingRunnerError::CargoFailed {
                crate_name: "<sigs-dump>".to_string(),
                stderr: err,
            });
        }
        // Atomic write.
        let tmp = sigs_dir.join("signatures.json.tmp");
        fs::write(&tmp, buf.as_bytes())?;
        fs::rename(&tmp, &json_path)?;
        write_stamp(&stamp, &self.fingerprint_hex, self.profile, "sigs")?;
        Ok(json_path)
    }

    /// `execvp` into the runner. On Unix, never returns on success.
    /// On Windows, spawns a child and propagates its exit code via
    /// `std::process::exit`.
    #[must_use]
    pub fn exec(runner: &Path, argv: &[OsString]) -> BindingRunnerError {
        // We deliberately don't use `unsafe { libc::execvp }` here —
        // the workspace forbids unsafe outside binding/native. A
        // child-process wait + exit produces the same observable
        // semantics for our callers.
        let mut cmd = Command::new(runner);
        cmd.args(&argv[1..]);
        cmd.env("GOSSAMER_IN_RUNNER", "1");
        match cmd.status() {
            Ok(status) => {
                std::process::exit(status.code().unwrap_or(127));
            }
            Err(err) => BindingRunnerError::Io(err),
        }
    }

    fn render_input(&self, profile: TmplProfile) -> RenderInput<'_> {
        // `BindingEntry` lives in the template crate, but our
        // `RenderedBinding` mirrors it. We have to materialise a
        // matching `Vec<BindingEntry>` and stash it on `self`'s
        // lifetime via a thread-local — but that's fragile. The
        // simpler approach: build the `Vec<BindingEntry>` here and
        // own it via a leaking helper. We side-step that by calling
        // through small adapter helpers on `RenderInput` so we just
        // hand a freshly-built slice.
        RenderInput {
            project_id: &self.project_id,
            fingerprint_hex: &self.fingerprint_hex,
            gossamer_root: &self.gossamer_root,
            bindings: leaked_entries(&self.bindings),
            profile,
        }
    }

    fn is_fresh(
        &self,
        artifact: &Path,
        stamp: &Path,
        kind: &str,
    ) -> Result<bool, BindingRunnerError> {
        if !artifact.exists() || !stamp.exists() {
            return Ok(false);
        }
        let Ok(stamp_text) = fs::read_to_string(stamp) else {
            return Ok(false);
        };
        if !stamp_text.contains(&self.fingerprint_hex)
            || !stamp_text.contains(self.profile.dir())
            || !stamp_text.contains(kind)
        {
            return Ok(false);
        }
        let artifact_mtime = artifact.metadata()?.modified()?;
        let max_dep_mtime = max_path_dep_mtime(&self.bindings)?;
        if let Some(dep_mtime) = max_dep_mtime
            && dep_mtime > artifact_mtime
        {
            return Ok(false);
        }
        Ok(true)
    }
}

/// Compiled-mode static-link companion to [`BindingRunner`].
#[derive(Debug)]
pub struct StaticBindingsLib {
    /// SHA-256 of the manifest's binding set.
    pub fingerprint: [u8; 32],
    /// 12-char hex prefix of [`Self::fingerprint`].
    pub fingerprint_hex: String,
    /// `<cache>/runners/<fp>/staticlib/`.
    pub workdir: PathBuf,
    /// Bindings to link into the staticlib.
    pub bindings: Vec<RenderedBinding>,
    /// Absolute path to the gossamer source tree.
    pub gossamer_root: PathBuf,
    /// Cargo profile.
    pub profile: Profile,
    /// Project id for cosmetic comments.
    pub project_id: String,
}

impl StaticBindingsLib {
    /// Constructs a staticlib build from the manifest. Returns
    /// `Ok(None)` if `[rust-bindings]` is empty.
    pub fn from_manifest(
        manifest: &Manifest,
        manifest_dir: &Path,
        gossamer_root: &Path,
        profile: Profile,
    ) -> io::Result<Option<Self>> {
        let cache = cache_root()?;
        Self::from_manifest_in(manifest, manifest_dir, gossamer_root, profile, &cache)
    }

    /// Same as [`Self::from_manifest`] but uses an explicit cache
    /// root instead of reading `GOSSAMER_CACHE` / `XDG_CACHE_HOME`.
    pub fn from_manifest_in(
        manifest: &Manifest,
        manifest_dir: &Path,
        gossamer_root: &Path,
        profile: Profile,
        cache_root: &Path,
    ) -> io::Result<Option<Self>> {
        if manifest.rust_bindings.is_empty() {
            return Ok(None);
        }
        let fingerprint = manifest.rust_binding_fingerprint(manifest_dir);
        let fingerprint_hex = hex_prefix(&fingerprint, 6);
        let workdir = cache_root
            .join("runners")
            .join(&fingerprint_hex)
            .join(SUBDIR_STATICLIB);
        fs::create_dir_all(&workdir)?;
        let bindings = render_bindings(&manifest.rust_bindings, manifest_dir);
        Ok(Some(Self {
            fingerprint,
            fingerprint_hex,
            workdir,
            bindings,
            gossamer_root: gossamer_root.to_path_buf(),
            profile,
            project_id: manifest.project.id.as_str().to_string(),
        }))
    }

    /// Path the staticlib lands at after `ensure_built`.
    #[must_use]
    pub fn archive_path(&self) -> PathBuf {
        self.workdir
            .join("target")
            .join(self.profile.dir())
            .join("libgos_static_bindings.a")
    }

    /// Idempotently builds the staticlib. Returns the path to the
    /// produced `.a` archive.
    pub fn ensure_built(&self) -> Result<PathBuf, BindingRunnerError> {
        fs::create_dir_all(&self.workdir)?;
        let _lock = AdvisoryLock::acquire(&self.workdir.join(".gos-build.lock"))?;

        let cargo_toml = self.workdir.join("Cargo.toml");
        let lib_rs = self.workdir.join("lib.rs");
        let input = RenderInput {
            project_id: &self.project_id,
            fingerprint_hex: &self.fingerprint_hex,
            gossamer_root: &self.gossamer_root,
            bindings: leaked_entries(&self.bindings),
            profile: self.profile.template_profile(),
        };
        write_if_different(&cargo_toml, &render_staticlib_cargo_toml(&input))?;
        write_if_different(&lib_rs, &render_staticlib_lib_rs(&input))?;

        let archive = self.archive_path();
        let stamp = self.workdir.join("stamp.json");
        if self.is_fresh(&archive, &stamp)? {
            return Ok(archive);
        }
        run_cargo_build(
            &cargo_toml,
            &self.workdir.join("target"),
            self.profile,
            "--lib",
            "",
            "<staticlib>",
        )?;
        write_stamp(&stamp, &self.fingerprint_hex, self.profile, "staticlib")?;
        Ok(archive)
    }

    fn is_fresh(&self, artifact: &Path, stamp: &Path) -> Result<bool, BindingRunnerError> {
        if !artifact.exists() || !stamp.exists() {
            return Ok(false);
        }
        let Ok(stamp_text) = fs::read_to_string(stamp) else {
            return Ok(false);
        };
        if !stamp_text.contains(&self.fingerprint_hex)
            || !stamp_text.contains(self.profile.dir())
            || !stamp_text.contains("staticlib")
        {
            return Ok(false);
        }
        let artifact_mtime = artifact.metadata()?.modified()?;
        let max_dep_mtime = max_path_dep_mtime(&self.bindings)?;
        if let Some(dep_mtime) = max_dep_mtime
            && dep_mtime > artifact_mtime
        {
            return Ok(false);
        }
        Ok(true)
    }
}

fn hex_prefix(bytes: &[u8], n: usize) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(n * 2);
    for b in bytes.iter().take(n) {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// JSON model of `signatures.json` produced by the sigs-dump bin.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct SignatureDump {
    /// All modules registered via `register_module!`.
    pub modules: Vec<DumpedModule>,
}

/// One module entry in the sigs-dump JSON.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DumpedModule {
    /// `module::path` declared by the binding.
    pub path: String,
    /// Module-level doc string (may be empty).
    pub doc: String,
    /// Items in declaration order.
    pub items: Vec<DumpedItem>,
}

/// One item entry in the sigs-dump JSON.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DumpedItem {
    /// Item name.
    pub name: String,
    /// Item-level doc string.
    pub doc: String,
    /// Parameter types.
    pub params: Vec<DumpedType>,
    /// Return type.
    pub ret: DumpedType,
}

/// Type description recorded in the sigs-dump JSON.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum DumpedType {
    /// `()`.
    #[serde(rename = "unit")]
    Unit,
    /// `bool`.
    #[serde(rename = "bool")]
    Bool,
    /// `i64`.
    #[serde(rename = "i64")]
    I64,
    /// `f64`.
    #[serde(rename = "f64")]
    F64,
    /// `char`.
    #[serde(rename = "char")]
    Char,
    /// `String` / `&str`.
    #[serde(rename = "string")]
    String,
    /// `(T1, T2, ...)`.
    #[serde(rename = "tuple")]
    Tuple {
        /// Element types.
        items: Vec<DumpedType>,
    },
    /// `Vec<T>`.
    #[serde(rename = "vec")]
    Vec {
        /// Element type.
        of: Box<DumpedType>,
    },
    /// `Option<T>`.
    #[serde(rename = "option")]
    Option {
        /// Inner type.
        of: Box<DumpedType>,
    },
    /// `Result<T, E>`.
    #[serde(rename = "result")]
    Result {
        /// `Ok` payload type.
        ok: Box<DumpedType>,
        /// `Err` payload type.
        err: Box<DumpedType>,
    },
    /// Opaque handle.
    #[serde(rename = "opaque")]
    Opaque {
        /// Opaque type name.
        name: String,
    },
    /// Untyped (`Value::Native` passthrough).
    #[serde(rename = "any")]
    Any,
}

/// Parses the sigs-dump JSON.
pub fn parse_signature_dump(text: &str) -> Result<SignatureDump, BindingRunnerError> {
    serde_json::from_str(text).map_err(|e| BindingRunnerError::BadSignatureJson(e.to_string()))
}

fn render_bindings(
    rust_bindings: &BTreeMap<String, RustBindingSpec>,
    manifest_dir: &Path,
) -> Vec<RenderedBinding> {
    rust_bindings
        .iter()
        .map(|(name, spec)| {
            let (cargo_dep_line, features, local_root) = render_one(name, spec, manifest_dir);
            RenderedBinding {
                crate_name: name.clone(),
                cargo_dep_line,
                features,
                local_root,
            }
        })
        .collect()
}

fn render_one(
    name: &str,
    spec: &RustBindingSpec,
    manifest_dir: &Path,
) -> (String, Vec<String>, Option<PathBuf>) {
    match spec {
        RustBindingSpec::Path {
            version,
            path,
            features,
            default_features,
        } => {
            let abs = if Path::new(path).is_absolute() {
                PathBuf::from(path)
            } else {
                manifest_dir.join(path)
            };
            let mut parts: Vec<String> = Vec::new();
            if let Some(v) = version {
                parts.push(format!("version = \"{}\"", v.minimum));
            }
            parts.push(toml_path_kv("path", &abs));
            push_cargo_features(&mut parts, features, *default_features);
            (
                format!("{name} = {{ {} }}", parts.join(", ")),
                features.clone(),
                Some(abs),
            )
        }
        RustBindingSpec::Git {
            version,
            url,
            reference,
            features,
            default_features,
        } => {
            let mut parts: Vec<String> = Vec::new();
            if let Some(v) = version {
                parts.push(format!("version = \"{}\"", v.minimum));
            }
            parts.push(format!("git = \"{url}\""));
            if let Some(r) = reference {
                match r {
                    GitRef::Branch(b) => parts.push(format!("branch = \"{b}\"")),
                    GitRef::Tag(t) => parts.push(format!("tag = \"{t}\"")),
                    GitRef::Rev(r) => parts.push(format!("rev = \"{r}\"")),
                }
            }
            push_cargo_features(&mut parts, features, *default_features);
            (
                format!("{name} = {{ {} }}", parts.join(", ")),
                features.clone(),
                None,
            )
        }
        RustBindingSpec::Crates {
            version,
            features,
            default_features,
        } => {
            let mut parts: Vec<String> = Vec::new();
            parts.push(format!("version = \"{}\"", version.minimum));
            push_cargo_features(&mut parts, features, *default_features);
            (
                format!("{name} = {{ {} }}", parts.join(", ")),
                features.clone(),
                None,
            )
        }
    }
}

/// Renders a `key = '...'` TOML pair using a single-quoted literal
/// string so backslashes (Windows `D:\a\...`), quotes, and other
/// escape-prone bytes round-trip unchanged. TOML literal strings
/// disallow `'` and ASCII control chars; if the path contains
/// either we fall back to a basic string with `\\` doubling, which
/// covers every realistic filesystem path on the platforms we
/// support without sacrificing correctness.
fn toml_path_kv(key: &str, path: &Path) -> String {
    let display = path.display().to_string();
    if !display.contains('\'') && !display.chars().any(char::is_control) {
        format!("{key} = '{display}'")
    } else {
        let escaped = display.replace('\\', "\\\\").replace('"', "\\\"");
        format!("{key} = \"{escaped}\"")
    }
}

fn push_cargo_features(parts: &mut Vec<String>, features: &[String], default_features: bool) {
    if !features.is_empty() {
        let listed: Vec<String> = features.iter().map(|f| format!("\"{f}\"")).collect();
        parts.push(format!("features = [{}]", listed.join(", ")));
    }
    if !default_features {
        parts.push("default-features = false".to_string());
    }
}

fn cache_root() -> io::Result<PathBuf> {
    if let Some(s) = std::env::var_os("GOSSAMER_CACHE") {
        return Ok(PathBuf::from(s).join("gossamer"));
    }
    if let Some(s) = std::env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(s).join("gossamer"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(home).join(".cache").join("gossamer"));
    }
    // Windows fallback: %LOCALAPPDATA% is the per-user cache root,
    // %USERPROFILE%\AppData\Local is its long form.
    if let Some(s) = std::env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(s).join("gossamer"));
    }
    if let Some(s) = std::env::var_os("USERPROFILE") {
        return Ok(PathBuf::from(s)
            .join("AppData")
            .join("Local")
            .join("gossamer"));
    }
    Err(io::Error::other(
        "cannot determine cache directory: set GOSSAMER_CACHE, XDG_CACHE_HOME, HOME, LOCALAPPDATA, or USERPROFILE",
    ))
}

fn write_if_different(path: &Path, contents: &str) -> io::Result<()> {
    if let Ok(existing) = fs::read_to_string(path)
        && existing == contents
    {
        return Ok(());
    }
    let tmp_path = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|s| s.to_str()).unwrap_or("dat")
    ));
    fs::write(&tmp_path, contents.as_bytes())?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn write_stamp(path: &Path, fingerprint_hex: &str, profile: Profile, kind: &str) -> io::Result<()> {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let body = format!(
        "{{\"fingerprint\":\"{fingerprint_hex}\",\"built_at\":{now},\"profile\":\"{}\",\"kind\":\"{kind}\"}}",
        profile.dir()
    );
    write_if_different(path, &body)
}

fn run_cargo_build(
    manifest_path: &Path,
    target_dir: &Path,
    profile: Profile,
    kind_flag: &str,
    kind_value: &str,
    crate_label: &str,
) -> Result<(), BindingRunnerError> {
    let cargo = which::which("cargo").map_err(|_| BindingRunnerError::CargoMissing)?;
    let mut cmd = Command::new(cargo);
    cmd.arg("build");
    if matches!(profile, Profile::Release) {
        cmd.arg("--release");
    }
    cmd.arg("--manifest-path").arg(manifest_path);
    if kind_value.is_empty() {
        cmd.arg(kind_flag);
    } else {
        cmd.arg(kind_flag).arg(kind_value);
    }
    cmd.env("CARGO_TARGET_DIR", target_dir);
    // Inherit stderr so cargo errors surface immediately. Capture
    // stdout so progress noise doesn't pollute the user's terminal
    // unless something fails.
    cmd.stderr(Stdio::piped());
    cmd.stdout(Stdio::piped());
    let mut child = cmd.spawn().map_err(BindingRunnerError::Io)?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_buf = collect_stream(stdout);
    let stderr_buf = collect_stream(stderr);
    let status = child.wait()?;
    let stdout_text = stdout_buf.lock().expect("poisoned").clone();
    let stderr_text = stderr_buf.lock().expect("poisoned").clone();
    if !status.success() {
        // Forward both streams so the user sees what went wrong.
        let _ = writeln!(io::stderr(), "{stdout_text}");
        let _ = writeln!(io::stderr(), "{stderr_text}");
        return Err(BindingRunnerError::CargoFailed {
            crate_name: crate_label.to_string(),
            stderr: stderr_text,
        });
    }
    Ok(())
}

fn collect_stream<R: Read + Send + 'static>(stream: Option<R>) -> std::sync::Arc<Mutex<String>> {
    let acc = std::sync::Arc::new(Mutex::new(String::new()));
    if let Some(mut s) = stream {
        let acc2 = acc.clone();
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            *acc2.lock().expect("poisoned") = buf;
        })
        .join()
        .ok();
    }
    acc
}

fn max_path_dep_mtime(bindings: &[RenderedBinding]) -> io::Result<Option<SystemTime>> {
    let mut best: Option<SystemTime> = None;
    for b in bindings {
        let Some(root) = &b.local_root else {
            continue;
        };
        if !root.exists() {
            continue;
        }
        walk_max_mtime(root, &mut best)?;
    }
    Ok(best)
}

fn walk_max_mtime(path: &Path, best: &mut Option<SystemTime>) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_dir() {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if name == "target" || name == ".git" || name.starts_with('.') {
            return Ok(());
        }
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            walk_max_mtime(&entry.path(), best)?;
        }
        return Ok(());
    }
    if let Ok(mtime) = meta.modified()
        && best.is_none_or(|b| mtime > b)
    {
        *best = Some(mtime);
    }
    Ok(())
}

fn leaked_entries(rendered: &[RenderedBinding]) -> &'static [BindingEntry] {
    // We pre-construct a slice of `BindingEntry` matching the
    // `RenderedBinding` layout. The template renderer only reads
    // it; no leak required since it lives on the heap inside a
    // `Box::leak` keyed by the rendered set's identity.
    use std::sync::OnceLock;
    static TABLE: OnceLock<
        std::sync::Mutex<std::collections::HashMap<usize, &'static [BindingEntry]>>,
    > = OnceLock::new();
    let table = TABLE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let key = rendered.as_ptr() as usize;
    let mut guard = table.lock().expect("poisoned");
    if let Some(v) = guard.get(&key) {
        return v;
    }
    let entries: Vec<BindingEntry> = rendered
        .iter()
        .map(|r| BindingEntry {
            crate_name: r.crate_name.clone(),
            cargo_dep_line: r.cargo_dep_line.clone(),
            features: r.features.clone(),
        })
        .collect();
    let leaked: &'static [BindingEntry] = Box::leak(entries.into_boxed_slice());
    guard.insert(key, leaked);
    leaked
}

/// Cross-process advisory lock.
///
/// We avoid pulling in `fs2` / `fd-lock` for one-shot use: a
/// best-effort exclusive create-on-open is sufficient for the
/// intended "two `gos` processes started seconds apart" case, and
/// it doesn't require the workspace to permit unsafe.
struct AdvisoryLock {
    path: PathBuf,
}

impl AdvisoryLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(mut f) => {
                    let _ = writeln!(f, "{}", std::process::id());
                    return Ok(Self {
                        path: path.to_path_buf(),
                    });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    if std::time::Instant::now() > deadline {
                        return Err(io::Error::other(format!(
                            "another `gos` process holds {} for >5 min",
                            path.display()
                        )));
                    }
                    // Best-effort: stale lock detection — if the PID
                    // in the file no longer exists, take it.
                    if let Ok(text) = fs::read_to_string(path)
                        && let Ok(pid) = text.trim().parse::<u32>()
                        && !pid_alive(pid)
                    {
                        let _ = fs::remove_file(path);
                        continue;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(err) => return Err(err),
            }
        }
    }
}

impl Drop for AdvisoryLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // /proc/<pid> exists ↔ pid is alive on Linux. On non-linux
        // unix we conservatively say "alive" so we don't steal a
        // valid lock.
        if cfg!(target_os = "linux") {
            return PathBuf::from(format!("/proc/{pid}")).exists();
        }
        true
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gossamer_pkg::Manifest;

    fn write_manifest(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("project.toml");
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn from_manifest_returns_none_on_empty_section() {
        let src = "[project]\nid = \"example.com/p\"\nversion = \"0.1.0\"\n";
        let m = Manifest::parse(src).unwrap();
        let out = BindingRunner::from_manifest(
            &m,
            std::env::temp_dir().as_path(),
            std::env::temp_dir().as_path(),
            Profile::Debug,
        )
        .unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn from_manifest_in_yields_runner_for_path_binding() {
        let cache = tempdir();
        let manifest_dir = tempdir();
        let echo_dir = manifest_dir.join("echo");
        fs::create_dir_all(&echo_dir).unwrap();
        let body = "[project]\nid = \"example.com/p\"\nversion = \"0.1.0\"\n\n[rust-bindings]\necho = { path = \"./echo\" }\n".to_string();
        write_manifest(&manifest_dir, &body);
        let m = Manifest::parse(&body).unwrap();
        let runner = BindingRunner::from_manifest_in(
            &m,
            &manifest_dir,
            Path::new("/fake"),
            Profile::Debug,
            &cache,
        )
        .unwrap()
        .expect("runner");
        assert_eq!(runner.bindings.len(), 1);
        assert_eq!(runner.bindings[0].crate_name, "echo");
        assert!(
            runner.bindings[0]
                .local_root
                .as_ref()
                .unwrap()
                .ends_with("echo")
        );
        assert!(runner.workdir.starts_with(&cache));
    }

    #[test]
    fn toml_path_kv_uses_literal_string_for_backslash_paths() {
        // Mimics a Windows GitHub runner path. TOML basic strings
        // would interpret `\a`/`\g` as escape sequences and fail
        // to parse — single-quoted literal strings preserve the
        // bytes verbatim. This is the regression gate for the
        // Windows CI failure observed 2026-04-30.
        let p = PathBuf::from("D:\\a\\gossamer\\gossamer/crates/gossamer-binding");
        let kv = toml_path_kv("path", &p);
        assert!(kv.starts_with("path = '"), "expected literal string, got: {kv}");
        // The whole expression must round-trip through cargo's
        // strict TOML parser inside a `{ ... }` inline table.
        let snippet = format!("[deps]\nfoo = {{ {kv} }}\n");
        let _: toml::Value = toml::from_str(&snippet)
            .expect("toml_path_kv output round-trips through strict TOML parser");
    }

    #[test]
    fn toml_path_kv_falls_back_to_basic_string_when_path_has_apostrophe() {
        // Single-quoted TOML literal strings disallow `'`; the
        // helper must fall back to a basic string with `\\` doubling.
        let p = PathBuf::from("/tmp/it's a path/echo");
        let kv = toml_path_kv("path", &p);
        assert!(kv.starts_with("path = \""), "expected basic string, got: {kv}");
        let snippet = format!("[deps]\nfoo = {{ {kv} }}\n");
        let _: toml::Value = toml::from_str(&snippet)
            .expect("apostrophe-path renders as escaped basic string");
    }

    #[test]
    fn write_if_different_is_idempotent() {
        let dir = tempdir();
        let p = dir.join("file.txt");
        write_if_different(&p, "abc").unwrap();
        let mtime1 = fs::metadata(&p).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        write_if_different(&p, "abc").unwrap();
        let mtime2 = fs::metadata(&p).unwrap().modified().unwrap();
        assert_eq!(mtime1, mtime2, "no rewrite when content unchanged");
        write_if_different(&p, "def").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "def");
    }

    #[test]
    fn parse_signature_dump_round_trips_minimal_input() {
        let json = r#"{"modules":[{"path":"echo","doc":"d","items":[{"name":"shout","doc":"","params":[{"kind":"string"}],"ret":{"kind":"string"}}]}]}"#;
        let parsed = parse_signature_dump(json).unwrap();
        assert_eq!(parsed.modules.len(), 1);
        assert_eq!(parsed.modules[0].path, "echo");
        assert_eq!(parsed.modules[0].items[0].name, "shout");
        assert!(matches!(parsed.modules[0].items[0].ret, DumpedType::String));
    }

    #[test]
    fn parse_signature_dump_handles_nested_types() {
        let json = r#"{"modules":[{"path":"m","doc":"","items":[{"name":"f","doc":"","params":[{"kind":"vec","of":{"kind":"i64"}}],"ret":{"kind":"result","ok":{"kind":"i64"},"err":{"kind":"string"}}}]}]}"#;
        let parsed = parse_signature_dump(json).unwrap();
        let item = &parsed.modules[0].items[0];
        assert!(
            matches!(&item.params[0], DumpedType::Vec { of } if matches!(**of, DumpedType::I64))
        );
        assert!(matches!(&item.ret, DumpedType::Result { .. }));
    }

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "gos-binding-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn rand_suffix() -> String {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{now:x}")
    }
}
