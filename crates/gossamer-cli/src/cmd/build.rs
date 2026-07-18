//! `gos build [PATH]` - emit a linked native executable.
//!
//! LLVM is the canonical native codegen backend. `gos build`
//! (debug) and `gos build --release` both lower MIR to LLVM IR.
//! The Cranelift backend is no longer a `gos build` target -
//! `gossamer-codegen-cranelift` is retained solely for the
//! in-process JIT used by `gossamer-interp` to compile hot
//! bytecode bodies. Any MIR shape the LLVM lowerer refuses
//! produces a hard `gos build` failure: a per-function
//! Cranelift fallback would silently introduce ABI divergence
//! between the JIT and the AOT path, so the CLI sets
//! `GOSSAMER_FAIL_ON_LLVM_FALLBACK=1` for itself before
//! invoking the driver.
//!
//! Two opt levels are exposed:
//!
//! - `gos build` (no `--release`): the lightweight correctness MIR
//!   pipeline followed by minimal `opt` and `llc -O0` codegen.
//! - `gos build --release`: the release MIR pipeline followed by
//!   integrated Clang `-O3` codegen with the audited target flags.
//!   PGO and `GOS_LLVM_SPLIT_TOOLS=1` retain the explicit `opt` plus
//!   `llc` pipeline when separate pass control is required.
//!
//! The driver crate's profile-aware frontend entry is the single dispatch
//! point; the selected build profile controls MIR and LLVM optimization.
//!
//! Native builds run the linked artifact through `cc` (POSIX) or
//! `rust-lld -flavor link` (Windows MSVC). A non-host `--target`
//! cross-builds through the same pipeline: it selects the target's
//! runtime archive and a target-appropriate linker - the conventional
//! GNU cross driver for a same-OS Linux cross, or rustup's `ld.lld`
//! for the host-agnostic static-musl path and OS-crossing ELF links.
//! Only `*-linux-*` targets cross-build; producing a foreign Mach-O
//! or PE needs an external SDK this toolchain does not bundle.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

use crate::loaders::profile_rss_stage;
use crate::paths::{
    default_unit_name, platform_exe_name, read_entry_source, resolve_entry_arg, resolve_output_path,
};
use gossamer_pkg::Edition;

/// User-selected native-build options collected at the CLI boundary.
pub(crate) struct BuildRequest<'a> {
    pub(crate) path: Option<PathBuf>,
    pub(crate) target: Option<&'a str>,
    pub(crate) link: LinkOptions,
    pub(crate) out_dir: Option<PathBuf>,
    pub(crate) timings: bool,
}

/// `gos build` dispatcher: walks the project root for a default
/// entry point when no path is supplied.
pub(crate) fn dispatch(mut request: BuildRequest<'_>) -> Result<()> {
    if let Err(err) = crate::binding_dispatch::ensure_external_signatures() {
        eprintln!("warning: failed to load rust-binding signatures: {err}");
    }
    let resolved = resolve_entry_arg(request.path.take())?;
    run(&resolved, &request)
}

/// Per-build link options assembled at the dispatch boundary and
/// passed through `try_native_build` → `link_posix` /
/// `link_windows_msvc`. Centralising these here keeps the
/// link-strategy decision in one place.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LinkOptions {
    /// True for `gos build --release` (LLVM `-O3`); drives static
    /// linking, strip, gc-sections.
    pub(crate) release: bool,
    /// True when the user passed `-g`. Suppresses strip; everything
    /// else stays the same.
    pub(crate) debug_info: bool,
    /// True when the user passed `--dynamic`. Forces the legacy
    /// dynamic-glibc link path even on Linux release builds.
    pub(crate) dynamic: bool,
}

impl LinkOptions {
    /// On Linux release builds, prefer the static-musl link when the
    /// rustup target is installed and the user did not opt out. The
    /// `MUSL_RUNTIME_LIB` bake records musl availability at CLI build
    /// time; [`musl_runtime_available`] re-checks at link time so a
    /// removed-since-build musl target degrades to the gnu link instead
    /// of failing.
    fn want_static_musl(self) -> bool {
        self.release
            && !self.dynamic
            && cfg!(target_os = "linux")
            && MUSL_RUNTIME_LIB.is_some()
            && musl_runtime_available()
    }

    /// Whether to strip symbols from the linked binary. Enabled
    /// unless the user explicitly requested debug info via `-g`.
    fn want_strip(self) -> bool {
        !self.debug_info
    }
}

/// Compile-time path to the musl runtime archive, or `None` when
/// the rustup `x86_64-unknown-linux-musl` target wasn't installed
/// at cli build time. Populated by `gossamer-cli/build.rs`.
const MUSL_RUNTIME_LIB: Option<&str> = option_env!("GOSSAMER_RUNTIME_LIB_PATH_MUSL");

/// The `*-unknown-linux-musl` rustup triple for `arch`. Cross builds
/// select the target's musl CRT and bindings archive from this rather
/// than the host arch, so a static-musl link follows the produced
/// binary's architecture.
fn musl_triple_for_arch(arch: &str) -> &'static str {
    match arch {
        "aarch64" => "aarch64-unknown-linux-musl",
        _ => "x86_64-unknown-linux-musl",
    }
}

/// rustup's self-contained musl CRT/libc directory under `sysroot`,
/// for the given `musl_triple`.
fn musl_self_contained_dir(sysroot: &Path, musl_triple: &str) -> PathBuf {
    sysroot
        .join("lib")
        .join("rustlib")
        .join(musl_triple)
        .join("lib")
        .join("self-contained")
}

/// `true` when the rustup `x86_64-unknown-linux-musl` self-contained CRT
/// directory exists right now. `MUSL_RUNTIME_LIB` is baked at CLI build
/// time, but the rustup target can be removed afterward; this re-checks
/// at link time so a stale bake falls back to the gnu link rather than
/// failing the build. Warns once when it falls back, since the user asked
/// (via `--release`) for the static-musl link. The result is cached - the
/// musl state cannot change within a single `gos build`.
fn musl_runtime_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        let present = rustc_sysroot().is_ok_and(|sysroot| {
            musl_self_contained_dir(&sysroot, musl_triple_for_arch(std::env::consts::ARCH)).exists()
        });
        if !present {
            eprintln!(
                "warning: the static-musl release link is unavailable \
                 (rustup target x86_64-unknown-linux-musl is not installed); \
                 linking dynamically against the host libc instead. Run \
                 `rustup target add x86_64-unknown-linux-musl` to restore \
                 self-contained release binaries."
            );
        }
        present
    })
}

/// Resolve where the linked binary should land. `--out-dir` wins
/// when supplied; otherwise the project-relative `target/` layout
/// rules. `target_is_windows` is the *produced binary's* OS (see
/// [`resolve_output_path`]), not necessarily the host's.
fn output_path(
    file: &Path,
    unit_name: &str,
    release: bool,
    out_dir: Option<&Path>,
    target_is_windows: bool,
) -> Result<PathBuf> {
    if let Some(dir) = out_dir {
        fs::create_dir_all(dir).map_err(|e| anyhow!("creating {}: {e}", dir.display()))?;
        return Ok(dir.join(platform_exe_name(unit_name, target_is_windows)));
    }
    resolve_output_path(file, unit_name, release, target_is_windows)
}

fn run(file: &PathBuf, request: &BuildRequest<'_>) -> Result<()> {
    let target = request.target;
    let opts = request.link;
    let release = opts.release;
    let out_dir = request.out_dir.as_deref();
    let timings = request.timings;
    let started = Instant::now();
    let mut build_timings = BuildTimings::default();
    warn_if_pgo_profile_is_stale(file);
    let edition = crate::paths::project_edition_for_entry(file);
    // Resolve `--target`. `None` or the host triple takes the host
    // build path. A registered, Linux-target triple cross-builds
    // through the same `try_native_build` pipeline; the codegen target
    // override makes the `-mtriple` passed to opt/llc, the i128 ABI
    // marshalling, and the incremental object-cache key all follow the
    // requested triple.
    let host = gossamer_driver::TargetTriple::host();
    let cross_target = match target {
        Some(triple) if triple != host.as_str() => {
            // Reject unknown triples here so the error is a clean parse
            // failure, not a linker blow-up.
            gossamer_driver::LinkerOptions::for_target(triple)
                .ok_or_else(|| anyhow!("unknown target `{triple}`"))?;
            // The cross output is always a Linux ELF; producing a macOS
            // Mach-O or Windows PE from another host needs an external
            // SDK this toolchain does not bundle.
            if resolve_link_target(Some(triple)).os != TargetOs::Linux {
                return Err(anyhow!(
                    "cross-compiling to `{triple}` is not supported; only \
                     `*-linux-*` targets can be cross-built (the produced \
                     binary is always Linux/ELF)"
                ));
            }
            gossamer_codegen_llvm::set_target_triple(triple.to_string());
            Some(triple)
        }
        _ => None,
    };

    let unit_name = default_unit_name(file);
    // `cross_target` is `Some` only for a validated `*-linux-*` triple (see
    // above), so a cross build's produced binary is never Windows even when
    // the host compiling it is.
    let target_is_windows = cross_target.is_none() && cfg!(windows);
    let out_path = output_path(file, &unit_name, release, out_dir, target_is_windows)?;
    let phase_started = Instant::now();
    let source = read_entry_source(file)?;
    build_timings.bundle = phase_started.elapsed();
    let phase_started = Instant::now();
    let build_key = build_artifact_key(file, &source, edition, cross_target, opts, &out_path);
    let stamp_path = build_stamp_path(file, &out_path);
    if let Some(outcome) = load_unchanged_build(&stamp_path, &out_path, &build_key) {
        build_timings.stamp = phase_started.elapsed();
        build_timings.total = started.elapsed();
        println!(
            "build: {bytes}B native executable at {path} ({note})",
            bytes = outcome.size,
            path = out_path.display(),
            note = outcome.note,
        );
        if timings {
            build_timings.print(true);
        }
        return Ok(());
    }
    build_timings.stamp = phase_started.elapsed();
    let _ = fs::remove_file(&stamp_path);

    let phase_started = Instant::now();
    let (sf, resolutions, table, tcx) = validate_source(file, source, edition, &mut build_timings)?;
    build_timings.frontend = phase_started.elapsed();
    profile_rss_stage("build_frontend_released");
    let checked = gossamer_driver::CheckedFrontend {
        edition,
        sf,
        resolutions,
        table,
        tcx,
    };
    let outcome = try_native_build(
        &unit_name,
        file,
        &out_path,
        opts,
        cross_target,
        checked,
        &mut build_timings,
    )
    .map_err(|err| anyhow!("build: {}", err.user_message()))?;
    store_successful_build(&stamp_path, &out_path, &build_key, &outcome);
    println!(
        "build: {bytes}B native executable at {path} ({note})",
        bytes = outcome.size,
        path = out_path.display(),
        note = outcome.note,
    );
    if timings {
        build_timings.total = started.elapsed();
        build_timings.print(false);
    }
    Ok(())
}

/// Wall-clock accounting for the native build critical path. The values are
/// deliberately emitted by the CLI rather than the driver so library callers
/// do not inherit a reporting policy.
#[derive(Default)]
struct BuildTimings {
    bundle: Duration,
    stamp: Duration,
    autoderive: Duration,
    comptime: Duration,
    frontend: Duration,
    parse: Duration,
    resolve: Duration,
    typecheck: Duration,
    exhaustiveness: Duration,
    arena_escape: Duration,
    parse_cache_hit: bool,
    body_count: usize,
    llvm_object_count: usize,
    cranelift_companion: bool,
    codegen: Duration,
    link: Duration,
    total: Duration,
}

impl BuildTimings {
    fn print(&self, cache_hit: bool) {
        println!(
            "build-timings: {{\"bundle_us\":{},\"stamp_us\":{},\"autoderive_us\":{},\"comptime_us\":{},\"frontend_us\":{},\"parse_us\":{},\"resolve_us\":{},\"typecheck_us\":{},\"exhaustiveness_us\":{},\"arena_escape_us\":{},\"parse_cache_hit\":{},\"body_count\":{},\"llvm_object_count\":{},\"cranelift_companion\":{},\"codegen_us\":{},\"link_us\":{},\"total_us\":{},\"final_artifact_cache_hit\":{cache_hit}}}",
            self.bundle.as_micros(),
            self.stamp.as_micros(),
            self.autoderive.as_micros(),
            self.comptime.as_micros(),
            self.frontend.as_micros(),
            self.parse.as_micros(),
            self.resolve.as_micros(),
            self.typecheck.as_micros(),
            self.exhaustiveness.as_micros(),
            self.arena_escape.as_micros(),
            self.parse_cache_hit,
            self.body_count,
            self.llvm_object_count,
            self.cranelift_companion,
            self.codegen.as_micros(),
            self.link.as_micros(),
            self.total.as_micros(),
        );
    }
}

const BUILD_STAMP_VERSION: &str = "gossamer-linked-artifact-v1";

/// Fingerprints everything available before the frontend that can affect the
/// linked artifact. The bundled source covers sibling modules, while project
/// metadata, tool/runtime identity, target flags, PGO inputs, and linker
/// environment prevent a successful artifact from surviving a relevant
/// configuration change.
fn build_artifact_key(
    file: &Path,
    source: &str,
    edition: Edition,
    target: Option<&str>,
    opts: LinkOptions,
    out_path: &Path,
) -> String {
    let mut hash = gossamer_pkg::sha256::Hasher::new();
    let mut add = |label: &str, bytes: &[u8]| {
        hash.update(label.as_bytes());
        hash.update(&[0]);
        hash.update(&(bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    add("version", BUILD_STAMP_VERSION.as_bytes());
    add("source", source.as_bytes());
    add("entry", file.to_string_lossy().as_bytes());
    add("output", out_path.to_string_lossy().as_bytes());
    add(
        "options",
        format!(
            "edition={edition:?}|target={}|release={}|debug={}|dynamic={}|reproducible={}",
            target.unwrap_or("host"),
            opts.release,
            opts.debug_info,
            opts.dynamic,
            gossamer_codegen_llvm::reproducible_enabled(),
        )
        .as_bytes(),
    );
    if let Ok(exe) = std::env::current_exe() {
        add("compiler", file_stamp_identity(&exe).as_bytes());
    }
    for name in ["project.toml", "gos.lock"] {
        if let Some(path) = find_ancestor_file(file, name)
            && let Ok(contents) = fs::read(&path)
        {
            add(name, &contents);
        }
    }
    for path in [
        option_env!("GOSSAMER_RUNTIME_LIB_PATH").map(PathBuf::from),
        MUSL_RUNTIME_LIB.map(PathBuf::from),
        std::env::var_os("GOS_RUNTIME_LIB").map(PathBuf::from),
    ]
    .into_iter()
    .flatten()
    {
        add("runtime", file_stamp_identity(&path).as_bytes());
    }
    // These variables cover LLVM selection and tuning, link-driver changes,
    // PGO, deployment targeting, and externally supplied runtime/bindings.
    for name in [
        "CC",
        "CFLAGS",
        "GOS_BUILD_CACHE",
        "GOS_LLC",
        "GOS_LLVM_CLANG",
        "GOS_LLVM_JOBS",
        "GOS_LLVM_MCPU",
        "GOS_LLVM_OPT",
        "GOS_LLVM_SPLIT_TOOLS",
        "GOS_PGO_COLLECT",
        "GOS_PGO_PROFILE",
        "GOS_RUNTIME_LIB",
        "MACOSX_DEPLOYMENT_TARGET",
        "RUSTFLAGS",
        "RUSTUP_TOOLCHAIN",
    ] {
        if let Some(value) = std::env::var_os(name) {
            add(name, value.to_string_lossy().as_bytes());
            if matches!(name, "GOS_PGO_PROFILE" | "GOS_RUNTIME_LIB") {
                add(
                    "env-file",
                    file_stamp_identity(Path::new(&value)).as_bytes(),
                );
            }
        }
    }
    match gossamer_codegen_llvm::pgo_mode() {
        Some(gossamer_codegen_llvm::PgoMode::Collect(path)) => {
            add("pgo-collect", path.to_string_lossy().as_bytes());
        }
        Some(gossamer_codegen_llvm::PgoMode::Profile(path)) => {
            add("pgo-profile", file_stamp_identity(&path).as_bytes());
        }
        None => {}
    }
    hash.finalize_hex()
}

fn find_ancestor_file(entry: &Path, name: &str) -> Option<PathBuf> {
    entry
        .parent()?
        .ancestors()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn file_stamp_identity(path: &Path) -> String {
    let mut text = path.to_string_lossy().into_owned();
    if let Ok(meta) = fs::metadata(path) {
        text.push_str(&format!("|len={}", meta.len()));
        if let Ok(modified) = meta.modified()
            && let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            text.push_str(&format!("|mtime={}", since_epoch.as_nanos()));
        }
    }
    text
}

fn build_stamp_path(entry: &Path, out_path: &Path) -> PathBuf {
    let root = find_ancestor_file(entry, "project.toml")
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .or_else(|| entry.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let output_key = gossamer_pkg::sha256::hex(out_path.to_string_lossy().as_bytes());
    root.join(".gos-cache")
        .join("link-stamps")
        .join(format!("{output_key}.stamp"))
}

fn load_unchanged_build(
    stamp_path: &Path,
    out_path: &Path,
    expected_key: &str,
) -> Option<NativeBuildOutcome> {
    let stamp = fs::read_to_string(stamp_path).ok()?;
    let mut lines = stamp.lines();
    if lines.next()? != BUILD_STAMP_VERSION || lines.next()? != expected_key {
        return None;
    }
    let expected_output_identity = lines.next()?;
    if file_stamp_identity(out_path) != expected_output_identity {
        return None;
    }
    let note = lines.next()?.to_string();
    let size = fs::metadata(out_path).ok()?.len();
    Some(NativeBuildOutcome {
        size,
        note: format!("{note}, unchanged"),
    })
}

fn store_successful_build(
    stamp_path: &Path,
    out_path: &Path,
    key: &str,
    outcome: &NativeBuildOutcome,
) {
    let Some(parent) = stamp_path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let contents = format!(
        "{BUILD_STAMP_VERSION}\n{key}\n{}\n{}\n",
        file_stamp_identity(out_path),
        outcome.note,
    );
    let tmp = stamp_path.with_extension(format!("tmp-{}", std::process::id()));
    if fs::write(&tmp, contents).is_ok() {
        let _ = fs::rename(&tmp, stamp_path);
    }
}

/// Parses, resolves, and typechecks `source`. Renders diagnostics
/// to stderr on failure and returns a hard error so the caller
/// stops before reaching codegen. On success drops the
/// `SourceMap` and diagnostic vectors before returning so peak
/// RSS during backend lowering reflects only the live frontend
/// artifacts.
fn validate_source(
    file: &Path,
    source: String,
    edition: Edition,
    timings: &mut BuildTimings,
) -> Result<(
    gossamer_ast::SourceFile,
    gossamer_resolve::Resolutions,
    gossamer_types::TypeTable,
    gossamer_types::TyCtxt,
)> {
    // Compile-time codegen pass for from_json/to_json (and friends).
    let phase_started = Instant::now();
    let augmented = gossamer_parse::autoderive::augment_source(&source);
    timings.autoderive = phase_started.elapsed();
    // The augmented source supersedes the file contents for every subsequent
    // frontend stage. Release the original before parsing so large generated
    // files do not overlap the resolver, type table, and backend artifacts.
    drop(source);
    // Comptime fold: evaluate `comptime` regions and splice in their
    // result literals so the native backend compiles a constant.
    let phase_started = Instant::now();
    let augmented = crate::comptime_fold::fold_comptime(augmented, &file.to_string_lossy())?;
    timings.comptime = phase_started.elapsed();
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), augmented);
    let render_opts = gossamer_diagnostics::RenderOptions {
        colour: crate::paths::stderr_supports_colour(),
    };
    // `build` runs the same authoritative front-end gate as `check` /
    // `run` - including exhaustiveness (a non-exhaustive `match` would
    // otherwise compile to a binary that segfaults on the unmatched arm)
    // and the canonical-`std`-path check (GR0005). Anything the gate
    // rejects must never reach codegen.
    let outcome =
        gossamer_driver::check_frontend_with_edition(map.source(file_id), file_id, edition);
    timings.parse = outcome.timings.parse;
    timings.resolve = outcome.timings.resolve;
    timings.typecheck = outcome.timings.typecheck;
    timings.exhaustiveness = outcome.timings.exhaustiveness;
    timings.arena_escape = outcome.timings.arena_escape;
    timings.parse_cache_hit = outcome.timings.parse_cache_hit;
    if !outcome.diagnostics.is_empty() {
        for diag in &outcome.diagnostics {
            eprintln!("{}", gossamer_diagnostics::render(diag, &map, render_opts));
        }
        return Err(anyhow!(
            "{} front-end error(s); refusing to build",
            outcome.diagnostics.len()
        ));
    }
    profile_rss_stage("build_frontend_checked");
    // Drop the source map before backend lowering so peak RSS reflects
    // only the live frontend artifacts.
    drop(map);
    let gossamer_driver::CheckedFrontend {
        edition: _,
        sf,
        resolutions,
        table,
        tcx,
    } = outcome.checked;
    Ok((sf, resolutions, table, tcx))
}

struct NativeBuildOutcome {
    size: u64,
    note: String,
}

/// Why the native-build path bailed. Each variant carries a pre-
/// formatted one-line reason suitable for user output.
pub(crate) enum NativeBuildError {
    /// Cranelift/MIR couldn't lower some construct.
    LowerFailed(String),
    /// Host `cc` ran but returned non-zero.
    LinkerFailed(String),
    /// Host `cc` (or `$CC`) was not executable.
    LinkerMissing(String),
    /// Filesystem error writing the object file or output binary.
    Io(anyhow::Error),
}

impl NativeBuildError {
    pub(crate) fn user_message(&self) -> String {
        match self {
            Self::LowerFailed(reason) => {
                format!("native codegen cannot yet lower this program: {reason}")
            }
            Self::LinkerFailed(reason) => format!("linker failed: {reason}"),
            Self::LinkerMissing(reason) => format!("linker unavailable: {reason}"),
            Self::Io(err) => format!("filesystem error during build: {err:#}"),
        }
    }
}

/// Locates `libgossamer_runtime.a` - the static library produced
/// by the `gossamer-runtime` crate with `crate-type =
/// ["staticlib", "rlib"]`. First tries `$GOS_RUNTIME_LIB`, then
/// walks up from the executable looking for `target/<profile>/`,
/// then finally from the manifest directory at build time.
///
/// Public to the crate so `cmd::env` can surface the resolved
/// path in `gos env`.
pub(crate) fn find_runtime_lib() -> std::result::Result<PathBuf, NativeBuildError> {
    if let Ok(env) = std::env::var("GOS_RUNTIME_LIB") {
        let p = PathBuf::from(env);
        if p.exists() {
            return Ok(p);
        }
    }
    let lib_names: &[&str] = if cfg!(target_env = "msvc") {
        &["gossamer_runtime.lib", "libgossamer_runtime.a"]
    } else {
        &["libgossamer_runtime.a", "gossamer_runtime.lib"]
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(baked) = option_env!("GOSSAMER_RUNTIME_LIB_PATH") {
        candidates.push(PathBuf::from(baked));
    }
    let mut push_with_names = |dir: &Path| {
        for name in lib_names {
            candidates.push(dir.join(name));
        }
    };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            push_with_names(parent);
            if let Some(grandparent) = parent.parent() {
                push_with_names(grandparent);
                push_with_names(&grandparent.join("lib"));
            }
        }
    }
    push_with_names(Path::new("target/release"));
    push_with_names(Path::new("target/debug"));
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    Err(NativeBuildError::LinkerMissing(format!(
        "runtime static lib not found (tried both libgossamer_runtime.a \
         and gossamer_runtime.lib); set GOS_RUNTIME_LIB or run \
         `cargo build --release --package gossamer-runtime`. tried: {candidates:?}"
    )))
}

/// The produced binary's OS family, resolved from the target triple
/// rather than the host `cfg!` so every link decision follows the
/// target.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetOs {
    Linux,
    MacOs,
    Windows,
    Other,
}

/// The produced binary's C runtime / environment, resolved from the
/// target triple.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetEnv {
    Gnu,
    Musl,
    Msvc,
    Other,
}

/// Resolved link context for a build: the target triple decomposed
/// into the OS / arch / env that drive linker and runtime-archive
/// selection, plus whether this is a cross build (target != host).
struct LinkTarget {
    triple: String,
    os: TargetOs,
    arch: &'static str,
    env: TargetEnv,
    is_cross: bool,
}

/// The host build machine's OS family.
fn host_os() -> TargetOs {
    match std::env::consts::OS {
        "linux" => TargetOs::Linux,
        "macos" => TargetOs::MacOs,
        "windows" => TargetOs::Windows,
        _ => TargetOs::Other,
    }
}

/// Resolves the link context for `target` (the host triple when
/// `None`). The OS / arch / env are parsed from the triple text so they
/// describe the produced binary, not the host.
fn resolve_link_target(target: Option<&str>) -> LinkTarget {
    let host = gossamer_driver::TargetTriple::host().as_str().to_string();
    let triple = target.map_or_else(|| host.clone(), str::to_string);
    let is_cross = triple != host;
    let os = if triple.contains("linux") {
        TargetOs::Linux
    } else if triple.contains("darwin") || triple.contains("apple") {
        TargetOs::MacOs
    } else if triple.contains("windows") {
        TargetOs::Windows
    } else {
        TargetOs::Other
    };
    let env = if triple.contains("musl") {
        TargetEnv::Musl
    } else if triple.contains("msvc") {
        TargetEnv::Msvc
    } else if triple.contains("gnu") {
        TargetEnv::Gnu
    } else {
        TargetEnv::Other
    };
    let arch = match triple.split('-').next().unwrap_or("") {
        "x86_64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        "riscv64" | "riscv64gc" => "riscv64",
        _ => "unknown",
    };
    LinkTarget {
        triple,
        os,
        arch,
        env,
        is_cross,
    }
}

/// Resolves the runtime archive built *for* `triple`, in priority
/// order, never falling back to the host archive: a cross link must
/// not pull host-arch objects into a foreign-arch binary.
///
/// 1. `GOS_RUNTIME_LIB_<TRIPLE>` env override (path must exist).
/// 2. The baked host-arch musl archive, only when `triple` is the
///    host arch's musl triple (the host == target musl case).
/// 3. `<gos-bin>/../lib/<triple>/libgossamer_runtime.a` (installed
///    toolchain layout).
/// 4. `target/<triple>/{release,debug}/libgossamer_runtime.a` (dev
///    tree).
fn find_runtime_lib_for_target(triple: &str) -> std::result::Result<PathBuf, NativeBuildError> {
    let env_key = format!(
        "GOS_RUNTIME_LIB_{}",
        triple.replace(['-', '.'], "_").to_uppercase()
    );
    if let Ok(p) = std::env::var(&env_key) {
        let p = PathBuf::from(p);
        if p.exists() {
            return Ok(p);
        }
    }
    if let Some(baked) = MUSL_RUNTIME_LIB
        && triple == musl_triple_for_arch(std::env::consts::ARCH)
    {
        let p = PathBuf::from(baked);
        if p.exists() {
            return Ok(p);
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(libdir) = exe
            .parent()
            .and_then(Path::parent)
            .map(|gp| gp.join("lib").join(triple))
    {
        let cand = libdir.join("libgossamer_runtime.a");
        if cand.exists() {
            return Ok(cand);
        }
    }
    for profile in ["release", "debug"] {
        let cand = Path::new("target")
            .join(triple)
            .join(profile)
            .join("libgossamer_runtime.a");
        if cand.exists() {
            return Ok(cand);
        }
    }
    Err(NativeBuildError::LinkerMissing(format!(
        "no runtime archive for target `{triple}`. Build it with \
         `cargo build --release --target {triple} -p gossamer-runtime`, \
         or set {env_key}."
    )))
}

/// The conventional GNU cross compiler driver for a same-OS Linux
/// cross. Honours the cargo `CARGO_TARGET_<TRIPLE>_LINKER` convention,
/// then `GOS_CROSS_CC`, then the Debian `<arch>-linux-gnu-gcc` package
/// spelling derived from the target arch.
fn cross_cc(lt: &LinkTarget) -> String {
    let key = format!(
        "CARGO_TARGET_{}_LINKER",
        lt.triple.replace(['-', '.'], "_").to_uppercase()
    );
    std::env::var(&key)
        .or_else(|_| std::env::var("GOS_CROSS_CC"))
        .unwrap_or_else(|_| format!("{}-linux-gnu-gcc", lt.arch))
}

/// An ELF/GNU-flavor lld linker for the *host* triple, alongside any
/// leading flavor-selection arguments the caller must pass before its own
/// flags. Used for OS-crossing ELF links, where the host `cc` / `ld`
/// cannot emit the target's object format.
///
/// Prefers rustup's pre-named `gcc-ld/ld.lld[.exe]` wrapper (present on
/// every host this was verified against - Linux, macOS). Some hosts
/// (observed: Windows) ship the underlying `rust-lld[.exe]` binary
/// directly under `bin/` without that pre-named per-flavor copy; the
/// universal lld driver supports selecting the same GNU/ELF behavior
/// explicitly via a leading `-flavor gnu` argument, so falling back to
/// the bare binary is equivalent, not a workaround - it's the exact
/// pattern `link_windows_msvc` already uses successfully with `-flavor
/// link` to drive the same binary as the MSVC-flavor linker.
fn locate_host_lld() -> std::result::Result<(PathBuf, &'static [&'static str]), NativeBuildError> {
    let sysroot = rustc_sysroot()?;
    let host = gossamer_driver::TargetTriple::host();
    let bin_dir = sysroot
        .join("lib")
        .join("rustlib")
        .join(host.as_str())
        .join("bin");
    for name in ["ld.lld", "ld.lld.exe"] {
        let candidate = bin_dir.join("gcc-ld").join(name);
        if candidate.exists() {
            return Ok((candidate, &[]));
        }
    }
    for name in ["rust-lld", "rust-lld.exe"] {
        let candidate = bin_dir.join(name);
        if candidate.exists() {
            return Ok((candidate, &["-flavor", "gnu"]));
        }
    }
    Err(NativeBuildError::LinkerMissing(format!(
        "no ELF-capable lld found under {} \
         (looked for gcc-ld/ld.lld[.exe] and rust-lld[.exe]; needed for OS-crossing ELF links)",
        bin_dir.display(),
    )))
}

fn try_native_build(
    unit_name: &str,
    input_path: &PathBuf,
    out_path: &PathBuf,
    opts: LinkOptions,
    target: Option<&str>,
    checked: gossamer_driver::CheckedFrontend,
    timings: &mut BuildTimings,
) -> std::result::Result<NativeBuildOutcome, NativeBuildError> {
    let lt = resolve_link_target(target);
    let tmp_dir =
        std::env::temp_dir().join(format!("gos-build-{}-{}", std::process::id(), unit_name));
    fs::create_dir_all(&tmp_dir)
        .map_err(|err| NativeBuildError::Io(anyhow!("creating {}: {err}", tmp_dir.display())))?;
    let phase_started = Instant::now();
    let (object_paths, object_triple) =
        emit_native_objects(unit_name, &tmp_dir, opts.release, checked, timings)?;
    timings.codegen = phase_started.elapsed();
    profile_rss_stage("build_backend_emitted");
    // Static-musl is chosen for a cross musl target (musl links
    // statically by construction) or for a host release that opted in.
    let static_musl = lt.env == TargetEnv::Musl || opts.want_static_musl();
    let runtime_lib = if lt.is_cross {
        find_runtime_lib_for_target(&lt.triple)?
    } else if opts.want_static_musl() {
        // The musl runtime archive lives at a baked path emitted by
        // `gossamer-cli/build.rs`. If `option_env!` resolved at cli
        // build time but the file has since been deleted, fall back
        // to the dynamic-glibc path so the build still produces a
        // working (just-not-portable) binary.
        let p = PathBuf::from(MUSL_RUNTIME_LIB.unwrap());
        if p.exists() { p } else { find_runtime_lib()? }
    } else {
        find_runtime_lib()?
    };
    // The bindings staticlib must match the main link's libc and arch:
    // a static-musl link cannot take a glibc-built archive (undefined
    // __res_init / open64 / gnu_get_libc_version), and a cross link
    // cannot take host-arch objects. Build the bindings for the target.
    let bindings_target: Option<String> = if static_musl {
        Some(musl_triple_for_arch(lt.arch).to_string())
    } else if lt.is_cross {
        Some(lt.triple.clone())
    } else {
        None
    };
    let bindings_archive = build_static_bindings_lib(opts.release, bindings_target.as_deref())
        .map_err(|err| {
            NativeBuildError::LinkerMissing(format!("rust-bindings staticlib: {err}"))
        })?;
    let mut extra_archives: Vec<PathBuf> = Vec::new();
    if let Some(p) = bindings_archive {
        extra_archives.push(p);
    }
    // PGO collect mode: the LLVM mid-end emits instrumented IR that
    // calls `__llvm_profile_write_file()` on exit. That symbol lives
    // in `libclang_rt.profile-x86_64.a`; without it the link fails
    // with undefined reference. We locate the archive next to the
    // LLVM toolchain and splice it into the link as an extra archive.
    let pgo = pgo_link_config();
    if pgo.collect_path.is_some() && opts.release {
        if let Some(proflib) = find_clang_rt_profile() {
            extra_archives.push(proflib);
        }
    }
    if std::env::var_os("GOS_LINK_VERBOSE").is_some() {
        eprintln!("gos build: runtime lib: {}", runtime_lib.display());
        eprintln!("gos build: objects: {object_paths:?}");
        eprintln!("gos build: extra archives: {extra_archives:?}");
    }
    // Windows-MSVC is the only PE path and never a cross target (we
    // refuse non-Linux cross targets earlier). Key it off the host
    // build env so a Windows-GNU `gos` keeps the mingw `link_posix`
    // path it uses today.
    let phase_started = Instant::now();
    let link_result = if !lt.is_cross && cfg!(all(windows, target_env = "msvc")) {
        link_windows_msvc(&object_paths, &runtime_lib, &extra_archives, out_path)
    } else if static_musl {
        link_posix_static_musl(
            &lt,
            &object_paths,
            &runtime_lib,
            &extra_archives,
            out_path,
            opts,
        )
    } else {
        link_posix(
            &lt,
            &object_paths,
            &runtime_lib,
            &extra_archives,
            out_path,
            opts,
        )
    };
    timings.link = phase_started.elapsed();
    // Keep the per-build temp dir (objects + IR) when dumping IR or when
    // explicitly preserving artifacts for post-mortem inspection on a
    // platform the developer can't reproduce locally.
    let keep_artifacts = std::env::var_os("GOS_LLVM_DUMP").is_some()
        || std::env::var_os("GOS_KEEP_BUILD_ARTIFACTS").is_some();
    if !keep_artifacts {
        let _ = fs::remove_dir_all(&tmp_dir);
    }
    let _ = input_path;
    link_result.map(|()| {
        if let Some(profile_path) = pgo.collect_path.as_deref() {
            print_pgo_collect_instructions(out_path, profile_path);
        }
        NativeBuildOutcome {
            size: fs::metadata(out_path).map_or(0, |m| m.len()),
            note: format!(
                "target {triple}{tag}{pgo}",
                triple = object_triple.as_deref().unwrap_or("unknown"),
                tag = if static_musl { ", static-musl" } else { "" },
                pgo = if pgo.collect_path.is_some() {
                    ", pgo-collect"
                } else if pgo.profile {
                    ", pgo-guided"
                } else {
                    ""
                },
            ),
        }
    })
}

struct PgoLinkConfig {
    collect_path: Option<PathBuf>,
    profile: bool,
}

fn pgo_link_config() -> PgoLinkConfig {
    let mode = gossamer_codegen_llvm::pgo_mode();
    match mode.as_ref() {
        Some(gossamer_codegen_llvm::PgoMode::Collect(path)) => PgoLinkConfig {
            collect_path: Some(path.clone()),
            profile: false,
        },
        Some(gossamer_codegen_llvm::PgoMode::Profile(_)) => PgoLinkConfig {
            collect_path: None,
            profile: true,
        },
        None => PgoLinkConfig {
            collect_path: std::env::var_os("GOS_PGO_COLLECT").map(PathBuf::from),
            profile: std::env::var_os("GOS_PGO_PROFILE").is_some(),
        },
    }
}

fn print_pgo_collect_instructions(binary: &Path, profile_path: &Path) {
    eprintln!("pgo: instrumented binary at {}", binary.display());
    eprintln!("pgo: run it, then:");
    eprintln!(
        "      llvm-profdata merge -output=default.profdata {}",
        profile_path.display()
    );
    eprintln!("      gos build --release --pgo-profile default.profdata [PATH]");
}

fn warn_if_pgo_profile_is_stale(source: &Path) {
    let Some(gossamer_codegen_llvm::PgoMode::Profile(profile)) = gossamer_codegen_llvm::pgo_mode()
    else {
        return;
    };
    let (Ok(profile_meta), Ok(source_meta)) = (fs::metadata(&profile), fs::metadata(source)) else {
        return;
    };
    let (Ok(profile_time), Ok(source_time)) = (profile_meta.modified(), source_meta.modified())
    else {
        return;
    };
    if pgo_profile_is_stale(profile_time, source_time) {
        eprintln!(
            "pgo: warning: profile {} predates source {}; rebuild it or verify it is intentional",
            profile.display(),
            source.display()
        );
    }
}

fn pgo_profile_is_stale(
    profile_time: std::time::SystemTime,
    source_time: std::time::SystemTime,
) -> bool {
    profile_time < source_time
}

/// Locates `libclang_rt.profile-*.a` alongside the active LLVM
/// toolchain. Needed when building an instrumented PGO binary
/// (`GOS_PGO_COLLECT`): the runtime exports `__llvm_profile_write_file`
/// which the instrumented IR calls on exit to flush raw profile data.
fn find_clang_rt_profile() -> Option<PathBuf> {
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return None;
    };
    let (lib_name, os_subdir) = if cfg!(target_os = "linux") {
        (format!("libclang_rt.profile-{arch}.a"), "linux")
    } else if cfg!(target_os = "macos") {
        // Apple's clang_rt drops the `-arch` suffix and uses an
        // `_osx` flavour name; macOS ships only one slice per
        // archive so the same file covers x86_64 and aarch64.
        ("libclang_rt.profile_osx.a".to_string(), "darwin")
    } else if cfg!(target_os = "windows") {
        (format!("clang_rt.profile-{arch}.lib"), "windows")
    } else {
        return None;
    };

    let mut candidates: Vec<PathBuf> = Vec::new();

    // Explicit user override: any caller can pin the archive
    // directly without us guessing.
    if let Ok(path) = std::env::var("GOS_LLVM_PROFILE_RT") {
        candidates.push(PathBuf::from(path));
    }

    // Probe relative to the configured `opt` / `llc`, since the
    // profile archive ships in the same toolchain layout. Walks
    // up to the LLVM prefix and joins `lib/clang/<ver>/lib/<os>`.
    if let Some(prefix) = std::env::var_os("GOS_LLVM_OPT").map(PathBuf::from)
        && let Some(bin_dir) = prefix.parent()
        && let Some(llvm_prefix) = bin_dir.parent()
    {
        for ver in ["18", "19", "20", "17"] {
            candidates.push(
                llvm_prefix
                    .join("lib")
                    .join("clang")
                    .join(ver)
                    .join("lib")
                    .join(os_subdir)
                    .join(&lib_name),
            );
        }
    }

    // Platform-default install paths.
    if cfg!(target_os = "linux") {
        for ver in ["18", "19", "20", "17"] {
            candidates.push(PathBuf::from(format!(
                "/usr/lib/llvm-{ver}/lib/clang/{ver}/lib/linux/{lib_name}"
            )));
        }
    } else if cfg!(target_os = "macos") {
        for ver in ["18", "19", "20", "17"] {
            candidates.push(PathBuf::from(format!(
                "/opt/homebrew/opt/llvm@{ver}/lib/clang/{ver}/lib/darwin/{lib_name}"
            )));
            candidates.push(PathBuf::from(format!(
                "/usr/local/opt/llvm@{ver}/lib/clang/{ver}/lib/darwin/{lib_name}"
            )));
        }
        candidates.push(PathBuf::from(format!(
            "/opt/homebrew/opt/llvm/lib/clang/lib/darwin/{lib_name}"
        )));
    } else if cfg!(target_os = "windows") {
        for ver in ["18", "19", "20", "17"] {
            candidates.push(PathBuf::from(format!(
                "C:\\msys64\\mingw64\\lib\\clang\\{ver}\\lib\\windows\\{lib_name}"
            )));
            candidates.push(PathBuf::from(format!(
                "C:\\msys64\\clang64\\lib\\clang\\{ver}\\lib\\windows\\{lib_name}"
            )));
            candidates.push(PathBuf::from(format!(
                "C:\\Program Files\\LLVM\\lib\\clang\\{ver}\\lib\\windows\\{lib_name}"
            )));
        }
    }

    for p in &candidates {
        if p.exists() {
            return Some(p.clone());
        }
    }
    eprintln!(
        "pgo: warning: {lib_name} not found - instrumented binary may fail to link.\n\
         Point GOS_LLVM_PROFILE_RT at the archive, or install LLVM 17-20 \
         with the compiler-rt profile component."
    );
    None
}

/// Builds the per-project `libgos_static_bindings.a` if the
/// project declares `[rust-bindings]`. Returns the archive path
/// or `None` when bindings are absent.
fn build_static_bindings_lib(
    release: bool,
    cargo_target: Option<&str>,
) -> std::result::Result<Option<PathBuf>, gossamer_driver::binding_runner::BindingRunnerError> {
    use gossamer_driver::binding_runner::{Profile as RunnerProfile, StaticBindingsLib};

    let project = crate::paths::project_context();
    // Mirror `dispatch_runner_if_needed`: a malformed manifest must
    // not silently degrade to "no bindings".
    let Some(manifest_result) = project.manifest_result() else {
        return Ok(None);
    };
    let manifest = match manifest_result {
        Ok(m) => m,
        Err(err) => {
            return Err(
                gossamer_driver::binding_runner::BindingRunnerError::Manifest(err.to_string()),
            );
        }
    };
    if manifest.rust_bindings.is_empty() {
        return Ok(None);
    }
    let manifest_dir = project.manifest_dir().unwrap_or_else(|| PathBuf::from("."));
    let Some(gossamer_root) = crate::binding_dispatch::locate_gossamer_root() else {
        return Ok(None);
    };
    let profile = if release {
        RunnerProfile::Release
    } else {
        RunnerProfile::Debug
    };
    let Some(lib) =
        StaticBindingsLib::from_manifest(manifest, &manifest_dir, &gossamer_root, profile)
            .map_err(gossamer_driver::binding_runner::BindingRunnerError::Io)?
    else {
        return Ok(None);
    };
    let lib = lib.with_cargo_target(cargo_target.map(str::to_string));
    let archive = lib.ensure_built()?;
    Ok(Some(archive))
}

/// Renders a `Command` as a single readable line (program + args) for
/// `GOS_LINK_VERBOSE` diagnostics. Not shell-escaped - meant for a
/// human reading why a link succeeded or failed, not re-execution.
fn render_command(cmd: &std::process::Command) -> String {
    let mut s = cmd.get_program().to_string_lossy().into_owned();
    for arg in cmd.get_args() {
        s.push(' ');
        s.push_str(&arg.to_string_lossy());
    }
    s
}

/// Prints the resolved link command to stderr when `GOS_LINK_VERBOSE`
/// is set. The exact `cc`/linker line + libraries is the single most
/// useful artifact when a native link fails on a platform the
/// developer can't reproduce locally (e.g. the `-ldl`/mingw break).
fn trace_link_command(cmd: &std::process::Command) {
    if std::env::var_os("GOS_LINK_VERBOSE").is_some() {
        eprintln!("gos build: link: {}", render_command(cmd));
    }
}

/// POSIX/macOS link path. On Linux release builds with the rustup
/// musl target installed and `--dynamic` not set, this routes through
/// `link_posix_static_musl` to produce a fully static binary.
/// Otherwise drives the host `cc` (or `$CC`) for a dynamic-glibc
/// link. macOS always takes the dynamic path (libSystem can't be
/// statically linked, by Apple policy).
fn link_posix(
    lt: &LinkTarget,
    object_paths: &[PathBuf],
    runtime_lib: &Path,
    extra_archives: &[PathBuf],
    out_path: &Path,
    opts: LinkOptions,
) -> std::result::Result<(), NativeBuildError> {
    // An OS-crossing link (a macOS / Windows host targeting Linux)
    // cannot use the host `cc` / `ld` to emit a foreign-OS ELF, so it
    // drives rustup's `ld.lld` against a target sysroot. musl targets
    // never reach here - they take the self-contained static path,
    // which needs no sysroot on any host.
    if lt.is_cross && host_os() != lt.os {
        return link_cross_gnu_lld(
            lt,
            object_paths,
            runtime_lib,
            extra_archives,
            out_path,
            opts,
        );
    }

    let cc = if lt.is_cross {
        cross_cc(lt)
    } else {
        std::env::var("CC").unwrap_or_else(|_| "cc".to_string())
    };
    let mut cmd = std::process::Command::new(&cc);
    if lt.os == TargetOs::MacOs {
        configure_macos_link_command(&mut cmd);
    }
    // Prefer a fast linker for a native host link only; a cross gcc
    // driver selects its own target linker, so mold/lld here would
    // target the host.
    // Linux: mold (3-8x faster than GNU ld). macOS: ld.lld from brew
    // llvm; `-fuse-ld=lld` tells Apple's clang driver to pick it up.
    if !lt.is_cross {
        match lt.os {
            TargetOs::Linux if which::which("mold").is_ok() => {
                cmd.arg("-fuse-ld=mold");
            }
            TargetOs::MacOs if which::which("ld.lld").is_ok() => {
                cmd.arg("-fuse-ld=lld");
            }
            _ => {}
        }
    }
    for p in object_paths {
        cmd.arg(p);
    }
    cmd.arg(runtime_lib);
    for archive in extra_archives {
        cmd.arg(archive);
    }
    cmd.arg("-o").arg(out_path);
    // `-ldl` only exists on Linux (libdl). macOS folds `dl*` into
    // `libSystem` and Windows/mingw has no `libdl` at all - passing
    // it on either fails the link ("cannot find -ldl"). `libpthread`
    // / `libm` resolve as real libs (winpthreads on mingw) or stub-
    // forwarders on every target, so we keep those.
    cmd.arg("-lpthread");
    if lt.os == TargetOs::Linux {
        cmd.arg("-ldl");
    }
    cmd.arg("-lm");
    if lt.os == TargetOs::Windows {
        // Windows-GNU `gos` drives mingw's `cc` directly, so unlike a
        // rustc-driven link it must name the Win32 import libraries the
        // Rust runtime staticlib references but mingw's default specs
        // don't auto-link: ws2_32 (mio sockets), bcrypt/advapi32
        // (getrandom / std RNG), userenv (env home dir), ntdll (std
        // internals). All are core mingw-w64 import libs. Listed after
        // the archives so the single-pass GNU linker resolves their
        // symbols. The Windows-MSVC path links via `link.exe`
        // (`link_windows_msvc`) and never reaches here.
        for lib in ["ws2_32", "bcrypt", "advapi32", "userenv", "ntdll"] {
            cmd.arg(format!("-l{lib}"));
        }
    }
    if !extra_archives.is_empty() {
        // The rust-bindings staticlib pulls in `gossamer-runtime`
        // as a transitive Cargo dep, which produces a second copy
        // of every `gos_rt_*` symbol alongside `libgossamer_runtime.a`.
        // Both copies come from the same source tree and are
        // functionally identical, so let the linker keep the first
        // definition rather than failing the link. macOS `ld64`
        // doesn't accept the GNU-ld spelling - the equivalent
        // there is `-Wl,-multiply_defined,suppress`.
        if lt.os == TargetOs::MacOs {
            cmd.arg("-Wl,-multiply_defined,suppress");
        } else {
            cmd.arg("-Wl,--allow-multiple-definition");
        }
    }
    if opts.want_strip() {
        // Drop DWARF debug sections + dead code but KEEP the symbol
        // table. Compiled-tier panic traces and SIGQUIT dumps unwind
        // the real machine stack and symbolicate through `.symtab`;
        // `--strip-all` would erase function names. macOS keeps global
        // symbols (gos functions are global) via the post-link
        // `strip -x`. `-dead_strip` is atom-based: it removes
        // unreachable code AND data atoms, so every local rodata atom
        // the codegen relies on must carry `N_NO_DEAD_STRIP` (see
        // `IntrinsicContext::intern_string`).
        if lt.os == TargetOs::MacOs {
            cmd.arg("-Wl,-dead_strip");
        } else {
            cmd.arg("-Wl,--strip-debug").arg("-Wl,--gc-sections");
        }
    }
    trace_link_command(&cmd);
    match cmd.status() {
        Ok(s) if s.success() => {
            if opts.want_strip() && lt.os == TargetOs::MacOs {
                let _ = std::process::Command::new("strip")
                    .arg("-x")
                    .arg(out_path)
                    .status();
            }
            set_executable(out_path).map_err(NativeBuildError::Io)?;
            Ok(())
        }
        Ok(s) => Err(NativeBuildError::LinkerFailed(format!(
            "{cc} exited with {s}"
        ))),
        Err(err) => Err(NativeBuildError::LinkerMissing(format!("{cc}: {err}"))),
    }
}

fn configure_macos_link_command(command: &mut std::process::Command) {
    let deployment_target = gossamer_driver::macos_deployment::effective_deployment_target();
    configure_macos_link_command_with_target(command, &deployment_target);
}

fn configure_macos_link_command_with_target(
    command: &mut std::process::Command,
    deployment_target: &str,
) {
    gossamer_driver::macos_deployment::set_command_deployment_target(command, deployment_target);
    command.arg(format!("-mmacosx-version-min={deployment_target}"));
}

/// OS-crossing gnu-dynamic link (a macOS / Windows host targeting
/// Linux). The host toolchain cannot emit a Linux ELF, so this drives
/// rustup's `ld.lld` against a user-supplied glibc sysroot
/// (`GOS_CROSS_SYSROOT`). musl targets do not take this path - they
/// link statically against rustup's self-contained CRT, which needs no
/// sysroot on any host, so a clear error steers the user there when no
/// sysroot is supplied.
fn link_cross_gnu_lld(
    lt: &LinkTarget,
    object_paths: &[PathBuf],
    runtime_lib: &Path,
    extra_archives: &[PathBuf],
    out_path: &Path,
    opts: LinkOptions,
) -> std::result::Result<(), NativeBuildError> {
    let Some(sysroot) = std::env::var_os("GOS_CROSS_SYSROOT") else {
        return Err(NativeBuildError::LinkerMissing(format!(
            "cross-linking the gnu-dynamic target `{triple}` from this host \
             needs a target glibc sysroot. Set GOS_CROSS_SYSROOT to an {arch} \
             Linux sysroot, or target `{musl}` instead (musl links statically \
             with no sysroot, on any host).",
            triple = lt.triple,
            arch = lt.arch,
            musl = musl_triple_for_arch(lt.arch),
        )));
    };
    let (linker, flavor_args) = locate_host_lld()?;
    let sysroot = PathBuf::from(sysroot);
    let mut cmd = std::process::Command::new(&linker);
    cmd.args(flavor_args)
        .arg("--sysroot")
        .arg(&sysroot)
        // Emit `.eh_frame_hdr` so the unwinder can locate FDEs through
        // `dl_iterate_phdr` for panic / SIGQUIT backtraces.
        .arg("--eh-frame-hdr")
        .arg("-o")
        .arg(out_path);
    for p in object_paths {
        cmd.arg(p);
    }
    cmd.arg(runtime_lib);
    for archive in extra_archives {
        cmd.arg(archive);
    }
    cmd.arg("-lc").arg("-lpthread").arg("-ldl").arg("-lm");
    if !extra_archives.is_empty() {
        cmd.arg("--allow-multiple-definition");
    }
    cmd.arg("--gc-sections");
    if opts.want_strip() {
        cmd.arg("--strip-debug");
    }
    trace_link_command(&cmd);
    match cmd.status() {
        Ok(s) if s.success() => {
            set_executable(out_path).map_err(NativeBuildError::Io)?;
            Ok(())
        }
        Ok(s) => Err(NativeBuildError::LinkerFailed(format!(
            "{} exited with {s}",
            linker.display()
        ))),
        Err(err) => Err(NativeBuildError::LinkerMissing(format!(
            "{}: {err}",
            linker.display()
        ))),
    }
}

/// Static-musl link path - invokes the rustup-shipped lld (see
/// [`locate_host_lld`]) against rustup's self-contained musl
/// CRT/libc/libunwind for the *target's* arch. Produces a statically-linked
/// ELF that runs on any Linux host of that arch regardless of glibc/musl
/// install or version. It is a host tool and emits ELF for any arch from
/// the input objects, so this path is the
/// host-agnostic cross route: any host with the target's rustup musl
/// CRT installed can produce the binary. The runtime archive is
/// resolved by the caller (baked for the host arch, or per-target for a
/// cross build); here we invoke the linker directly so we don't need
/// `cc` to know about musl.
fn link_posix_static_musl(
    lt: &LinkTarget,
    object_paths: &[PathBuf],
    runtime_lib: &Path,
    extra_archives: &[PathBuf],
    out_path: &Path,
    opts: LinkOptions,
) -> std::result::Result<(), NativeBuildError> {
    let sysroot = rustc_sysroot()?;
    let target_musl = musl_triple_for_arch(lt.arch);
    let self_contained = musl_self_contained_dir(&sysroot, target_musl);
    if !self_contained.exists() {
        return Err(NativeBuildError::LinkerMissing(format!(
            "musl self-contained dir not found: {}; \
             try `rustup target add {target_musl}` \
             or pass `--dynamic` to `gos build --release`",
            self_contained.display(),
        )));
    }
    let (linker, flavor_args) = locate_host_lld()?;

    let mut cmd = std::process::Command::new(&linker);
    cmd.args(flavor_args)
        .arg("--static")
        // Emit `.eh_frame_hdr` + the `PT_GNU_EH_FRAME` program header.
        // The unwinder (`_Unwind_Backtrace`, used by the `backtrace`
        // crate for panic / SIGQUIT traces) locates FDEs through this
        // index via `dl_iterate_phdr`; without it the table-driven
        // unwind finds nothing and a backtrace yields zero frames.
        // The `cc`-driven dynamic link path passes this implicitly;
        // invoking `ld.lld` directly here does not, so it is explicit.
        .arg("--eh-frame-hdr")
        .arg("-o")
        .arg(out_path)
        .arg(self_contained.join("crt1.o"))
        .arg(self_contained.join("crti.o"));
    for p in object_paths {
        cmd.arg(p);
    }
    cmd.arg(runtime_lib);
    for archive in extra_archives {
        cmd.arg(archive);
    }
    cmd.arg(self_contained.join("libc.a"))
        .arg(self_contained.join("libunwind.a"))
        .arg(self_contained.join("crtn.o"));
    if !extra_archives.is_empty() {
        cmd.arg("--allow-multiple-definition");
    }
    cmd.arg("--gc-sections");
    if opts.want_strip() {
        // Keep `.symtab` so panic / SIGQUIT backtraces symbolicate
        // gos function names; only drop DWARF debug sections. See the
        // matching note in `link_posix`.
        cmd.arg("--strip-debug");
    }
    trace_link_command(&cmd);
    match cmd.status() {
        Ok(s) if s.success() => {
            set_executable(out_path).map_err(NativeBuildError::Io)?;
            Ok(())
        }
        Ok(s) => Err(NativeBuildError::LinkerFailed(format!(
            "{} exited with {s}",
            linker.display()
        ))),
        Err(err) => Err(NativeBuildError::LinkerMissing(format!(
            "{}: {err}",
            linker.display()
        ))),
    }
}

/// Resolves `rustc --print sysroot` once per process, as a `PathBuf`.
fn rustc_sysroot() -> std::result::Result<PathBuf, NativeBuildError> {
    static SYSROOT: std::sync::OnceLock<std::result::Result<PathBuf, String>> =
        std::sync::OnceLock::new();
    SYSROOT
        .get_or_init(|| {
            let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
            let out = std::process::Command::new(&rustc)
                .args(["--print", "sysroot"])
                .output()
                .map_err(|err| format!("rustc --print sysroot: {err}"))?;
            if !out.status.success() {
                return Err(format!("rustc --print sysroot exited with {}", out.status));
            }
            Ok(PathBuf::from(
                String::from_utf8_lossy(&out.stdout).trim().to_string(),
            ))
        })
        .clone()
        .map_err(NativeBuildError::LinkerMissing)
}

/// Windows MSVC link path - invokes `rust-lld -flavor link` with
/// MSVC-style flags. `cc` on Windows runners typically resolves to
/// MinGW gcc, which can't link MSVC-ABI rlibs (the runtime is built
/// against `windows-msvc`). `rust-lld.exe` ships with every rustup
/// toolchain and speaks the MSVC link.exe interface, so we don't
/// need vcvars or a pre-installed Visual Studio link.exe in PATH.
#[cfg(windows)]
fn link_windows_msvc(
    object_paths: &[PathBuf],
    runtime_lib: &Path,
    extra_archives: &[PathBuf],
    out_path: &Path,
) -> std::result::Result<(), NativeBuildError> {
    let linker = locate_rust_lld()?;
    let mut cmd = std::process::Command::new(&linker);
    cmd.arg("-flavor").arg("link").arg("/NOLOGO");
    let mut out_arg = std::ffi::OsString::from("/OUT:");
    out_arg.push(out_path);
    cmd.arg(out_arg);
    for p in object_paths {
        cmd.arg(p);
    }
    cmd.arg(runtime_lib);
    for archive in extra_archives {
        cmd.arg(archive);
    }
    if !extra_archives.is_empty() {
        // rust-bindings staticlibs pull in
        // `gossamer-runtime` as a transitive Cargo dep, producing a
        // second copy of every `gos_rt_*` symbol alongside the
        // primary `gossamer_runtime.lib`. Both copies are
        // functionally identical (same source tree). `/FORCE:MULTIPLE`
        // is the MSVC linker's equivalent of GNU ld's
        // `--allow-multiple-definition`; without it, `link.exe`
        // exits with LNK4006 ("multiply defined").
        cmd.arg("/FORCE:MULTIPLE");
    }
    for lib in [
        "advapi32.lib",
        "bcrypt.lib",
        "kernel32.lib",
        "ntdll.lib",
        "userenv.lib",
        "ws2_32.lib",
        "synchronization.lib",
        "dbghelp.lib",
        "msvcrt.lib",
        "ucrt.lib",
        "vcruntime.lib",
        "legacy_stdio_definitions.lib",
    ] {
        cmd.arg(lib);
    }
    trace_link_command(&cmd);
    match cmd.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(NativeBuildError::LinkerFailed(format!(
            "{} exited with {s}",
            linker.display()
        ))),
        Err(err) => Err(NativeBuildError::LinkerMissing(format!(
            "{}: {err}",
            linker.display()
        ))),
    }
}

#[cfg(not(windows))]
fn link_windows_msvc(
    _object_paths: &[PathBuf],
    _runtime_lib: &Path,
    _extra_archives: &[PathBuf],
    _out_path: &Path,
) -> std::result::Result<(), NativeBuildError> {
    Err(NativeBuildError::LinkerMissing(
        "Windows MSVC link path is only available on a Windows host".to_string(),
    ))
}

/// Finds `rust-lld.exe` inside the active rustup toolchain. Asks
/// `rustc --print sysroot` rather than guessing the toolchain path.
#[cfg(windows)]
fn locate_rust_lld() -> std::result::Result<PathBuf, NativeBuildError> {
    let candidate = rustc_sysroot()?
        .join("lib")
        .join("rustlib")
        .join("x86_64-pc-windows-msvc")
        .join("bin")
        .join("rust-lld.exe");
    if candidate.exists() {
        return Ok(candidate);
    }
    Ok(PathBuf::from("rust-lld.exe"))
}

/// Lowers the checked frontend into one or two object files under `tmp_dir`,
/// picking the codegen tier from `release`. Returns the object
/// paths plus the recorded target triple for the linker step.
fn emit_native_objects(
    unit_name: &str,
    tmp_dir: &Path,
    release: bool,
    checked: gossamer_driver::CheckedFrontend,
    timings: &mut BuildTimings,
) -> std::result::Result<(Vec<PathBuf>, Option<String>), NativeBuildError> {
    // LLVM is the canonical native backend. Strict-lowering is
    // default-on (`STRICT_LOWERING = true`) so any
    // `BuildError::Unsupported` from a body lowering is a hard
    // top-level error rather than a silent per-function
    // Cranelift fallback. `gos build --release` re-asserts that
    // default explicitly here so callers cannot accidentally see
    // a fallback-tier release binary.
    gossamer_codegen_llvm::set_strict_lowering(true);
    gossamer_codegen_llvm::set_opt_profile(if release {
        gossamer_codegen_llvm::OptProfile::Release
    } else {
        gossamer_codegen_llvm::OptProfile::Debug
    });
    // Anchor the incremental cache next to the project when possible
    // so repeated `gos build` invocations share a warm cache.
    let cache_dir = std::env::current_dir()
        .ok()
        .map(|d| d.join(".gos-cache").join("ir-cache"));
    if let Some(ref cd) = cache_dir {
        gossamer_codegen_llvm::set_cache_dir(cd.clone());
    }
    // Per-body LLVM objects land in their own subdirectory so the
    // Cranelift companion sits alongside without filename collisions.
    let llvm_obj_dir = tmp_dir.join("llvm");
    fs::create_dir_all(&llvm_obj_dir)
        .map_err(|e| NativeBuildError::Io(anyhow!("creating {}: {e}", llvm_obj_dir.display())))?;
    let cl_path = tmp_dir.join(format!("{unit_name}.cl.o"));
    let build =
        gossamer_driver::compile_at_paths_from_frontend(checked, &llvm_obj_dir, &cl_path, release)
            .map_err(|err| NativeBuildError::LowerFailed(err.to_string()))?;
    timings.body_count = build.body_count;
    timings.llvm_object_count = build.llvm_object_count;
    timings.cranelift_companion = build.has_cranelift_companion;
    let mut object_paths: Vec<PathBuf> = build.llvm_objects;
    if build.has_cranelift_companion {
        object_paths.push(cl_path);
        eprintln!(
            "build: per-function Cranelift companion engaged for {n} bodies: {names:?}",
            n = build.fallback_bodies.len(),
            names = build.fallback_bodies,
        );
    }
    Ok((object_paths, Some(build.triple)))
}

// The `Result` is load-bearing on unix (the `chmod` below can fail); on
// non-unix the body is infallible, which is the only configuration where
// clippy sees an always-`Ok` return.
#[allow(clippy::unnecessary_wraps)]
fn set_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        use anyhow::Context;

        use crate::paths::friendly_io_error;

        let meta = fs::metadata(path).map_err(|e| friendly_io_error(e, path))?;
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o111);
        fs::set_permissions(path, perms).with_context(|| format!("chmod +x {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    fn scratch(name: &str) -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "gossamer-build-test-{}-{id}-{name}",
            std::process::id()
        ))
    }

    #[test]
    fn pgo_profile_staleness_uses_strict_timestamp_ordering() {
        let older = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1);
        let newer = std::time::UNIX_EPOCH + std::time::Duration::from_secs(2);
        assert!(super::pgo_profile_is_stale(older, newer));
        assert!(!super::pgo_profile_is_stale(newer, older));
        assert!(!super::pgo_profile_is_stale(older, older));
    }

    #[test]
    fn render_command_joins_program_and_args() {
        let mut cmd = std::process::Command::new("cc");
        cmd.arg("a.o").arg("-o").arg("out").arg("-lpthread");
        assert_eq!(super::render_command(&cmd), "cc a.o -o out -lpthread");
    }

    #[test]
    fn macos_link_command_uses_supported_deployment_target() {
        let mut cmd = std::process::Command::new("cc");
        super::configure_macos_link_command_with_target(
            &mut cmd,
            gossamer_driver::macos_deployment::DEFAULT_MACOSX_DEPLOYMENT_TARGET,
        );

        assert!(
            cmd.get_args().any(|arg| arg == "-mmacosx-version-min=15.0"),
            "link command missing macOS 15 deployment flag: {}",
            super::render_command(&cmd)
        );
        let deployment_target = cmd
            .get_envs()
            .find(|(name, _)| {
                *name == gossamer_driver::macos_deployment::MACOSX_DEPLOYMENT_TARGET_ENV
            })
            .and_then(|(_, value)| value)
            .expect("link command deployment target environment");
        assert_eq!(deployment_target, "15.0");
    }

    #[test]
    fn musl_triple_for_arch_selects_by_arch() {
        assert_eq!(
            super::musl_triple_for_arch("aarch64"),
            "aarch64-unknown-linux-musl"
        );
        assert_eq!(
            super::musl_triple_for_arch("x86_64"),
            "x86_64-unknown-linux-musl"
        );
    }

    #[test]
    fn cross_link_target_decodes_aarch64_musl() {
        let lt = super::resolve_link_target(Some("aarch64-unknown-linux-musl"));
        assert_eq!(lt.arch, "aarch64");
        assert!(matches!(lt.os, super::TargetOs::Linux));
        assert!(matches!(lt.env, super::TargetEnv::Musl));
        // A non-host triple is always a cross build, on any host.
        assert!(lt.is_cross);
    }

    #[test]
    fn cross_link_target_decodes_x86_64_musl() {
        let lt = super::resolve_link_target(Some("x86_64-unknown-linux-musl"));
        assert_eq!(lt.arch, "x86_64");
        assert!(matches!(lt.os, super::TargetOs::Linux));
        assert!(matches!(lt.env, super::TargetEnv::Musl));
    }

    #[test]
    fn host_link_target_is_not_cross() {
        let lt = super::resolve_link_target(None);
        assert!(!lt.is_cross);
        match std::env::consts::ARCH {
            "x86_64" => assert_eq!(lt.arch, "x86_64"),
            "aarch64" => assert_eq!(lt.arch, "aarch64"),
            _ => {}
        }
    }

    #[test]
    fn cross_runtime_lookup_errors_when_absent() {
        // A bogus triple with no archive must error, never silently
        // return the host (x86) archive for a foreign-arch link.
        let r = super::find_runtime_lib_for_target("aarch64-unknown-linux-gnu-bogus-nonexistent");
        assert!(r.is_err());
    }

    #[test]
    fn linked_artifact_stamp_hits_and_invalidates_with_output() {
        let root = scratch("stamp");
        std::fs::create_dir_all(&root).expect("create scratch");
        let output = root.join("app");
        let stamp = root.join("app.stamp");
        std::fs::write(&output, b"binary").expect("write output");
        let outcome = super::NativeBuildOutcome {
            size: 6,
            note: "test link".to_string(),
        };
        super::store_successful_build(&stamp, &output, "key-a", &outcome);
        let hit = super::load_unchanged_build(&stamp, &output, "key-a").expect("stamp hit");
        assert_eq!(hit.size, 6);
        assert!(hit.note.contains("unchanged"));
        assert!(super::load_unchanged_build(&stamp, &output, "key-b").is_none());

        std::fs::write(&output, b"changed binary").expect("replace output");
        assert!(
            super::load_unchanged_build(&stamp, &output, "key-a").is_none(),
            "an externally replaced artifact must not be accepted"
        );
        std::fs::remove_dir_all(root).expect("remove scratch");
    }

    #[test]
    fn linked_artifact_key_changes_with_source_and_profile() {
        let root = scratch("key");
        std::fs::create_dir_all(&root).expect("create scratch");
        let entry = root.join("main.gos");
        let output = root.join("app");
        std::fs::write(&entry, b"fn main() {}\n").expect("write entry");
        let debug = super::LinkOptions {
            release: false,
            debug_info: false,
            dynamic: true,
        };
        let release = super::LinkOptions {
            release: true,
            debug_info: false,
            dynamic: true,
        };
        let first = super::build_artifact_key(
            &entry,
            "fn main() {}\n",
            gossamer_pkg::Edition::E2026,
            None,
            debug,
            &output,
        );
        let source_changed = super::build_artifact_key(
            &entry,
            "fn main() { println(\"changed\") }\n",
            gossamer_pkg::Edition::E2026,
            None,
            debug,
            &output,
        );
        let profile_changed = super::build_artifact_key(
            &entry,
            "fn main() {}\n",
            gossamer_pkg::Edition::E2026,
            None,
            release,
            &output,
        );
        assert_ne!(first, source_changed);
        assert_ne!(first, profile_changed);
        std::fs::remove_dir_all(root).expect("remove scratch");
    }
}
