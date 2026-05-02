//! `gos build [PATH]` — emit a linked native executable.
//!
//! Two codegen tiers cooperate:
//!
//! - Default (`gos build`): Cranelift end-to-end. Fast compile,
//!   modest runtime perf.
//! - `--release`: LLVM at `-O3` with per-function fallback to
//!   Cranelift for any body the LLVM lowerer cannot cover yet.
//!   The two objects are linked together so a partial-LLVM
//!   module still gets the optimised path on the bodies it
//!   accepts.
//!
//! Native (host) builds run the linked artifact through `cc`
//! (POSIX) or `rust-lld -flavor link` (Windows MSVC). Cross
//! targets fall through to the platform-agnostic byte-stream
//! artifact path until cross-codegen ships.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::paths::{default_main_entry, default_unit_name, read_source, resolve_output_path};

/// `gos build` dispatcher: walks the project root for a default
/// entry point when no path is supplied.
pub(crate) fn dispatch(
    path: Option<PathBuf>,
    target: Option<&str>,
    release: bool,
    debug_info: bool,
    dynamic: bool,
) -> Result<()> {
    if let Err(err) = crate::binding_dispatch::ensure_external_signatures() {
        eprintln!("warning: failed to load rust-binding signatures: {err}");
    }
    let resolved = match path {
        Some(p) => p,
        None => default_main_entry()?,
    };
    run(&resolved, target, release, debug_info, dynamic)
}

/// Per-build link options assembled at the dispatch boundary and
/// passed through `try_native_build` → `link_posix` /
/// `link_windows_msvc`. Centralising these here keeps the
/// link-strategy decision in one place.
#[derive(Debug, Clone, Copy)]
struct LinkOptions {
    /// True for `gos build --release` (LLVM `-O3`); drives static
    /// linking, strip, gc-sections.
    release: bool,
    /// True when the user passed `-g`. Suppresses strip; everything
    /// else stays the same.
    debug_info: bool,
    /// True when the user passed `--dynamic`. Forces the legacy
    /// dynamic-glibc link path even on Linux release builds.
    dynamic: bool,
}

impl LinkOptions {
    /// On Linux release builds, prefer the static-musl link when
    /// the rustup target is installed and the user did not opt out.
    fn want_static_musl(self) -> bool {
        self.release && !self.dynamic && cfg!(target_os = "linux") && MUSL_RUNTIME_LIB.is_some()
    }

    /// Whether to pass `-Wl,--strip-all` to the linker. Stripping
    /// only kicks in for release builds without `-g`.
    fn want_strip(self) -> bool {
        self.release && !self.debug_info
    }
}

/// Compile-time path to the musl runtime archive, or `None` when
/// the rustup `x86_64-unknown-linux-musl` target wasn't installed
/// at cli build time. Populated by `gossamer-cli/build.rs`.
const MUSL_RUNTIME_LIB: Option<&str> = option_env!("GOSSAMER_RUNTIME_LIB_PATH_MUSL");

fn run(
    file: &PathBuf,
    target: Option<&str>,
    release: bool,
    debug_info: bool,
    dynamic: bool,
) -> Result<()> {
    let source = read_source(file)?;

    // Validate source before attempting any codegen.  A broken AST or
    // unresolved name must fail the build immediately rather than
    // producing a segfaulting native binary or a launcher that
    // panics at runtime.
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let render_opts = gossamer_diagnostics::RenderOptions {
        colour: crate::paths::stderr_supports_colour(),
    };
    let (sf, parse_diags) = gossamer_parse::parse_source_file(&source, file_id);
    if !parse_diags.is_empty() {
        for diag in &parse_diags {
            let structured = diag.to_diagnostic();
            eprintln!(
                "{}",
                gossamer_diagnostics::render(&structured, &map, render_opts)
            );
        }
        return Err(anyhow!(
            "{} parse error(s); refusing to build",
            parse_diags.len()
        ));
    }
    let (resolutions, resolve_diags) = gossamer_resolve::resolve_source_file(&sf);
    let in_scope: Vec<&str> = crate::loaders::collect_top_level_names(&sf);
    let unresolved: Vec<_> = resolve_diags
        .iter()
        .filter(|d| {
            matches!(
                d.error,
                gossamer_resolve::ResolveError::UnresolvedName { .. }
                    | gossamer_resolve::ResolveError::DuplicateItem { .. }
            )
        })
        .collect();
    if !unresolved.is_empty() {
        for diag in &unresolved {
            let structured = diag.to_diagnostic(&in_scope);
            eprintln!(
                "{}",
                gossamer_diagnostics::render(&structured, &map, render_opts)
            );
        }
        return Err(anyhow!(
            "{} resolve error(s); refusing to build",
            unresolved.len()
        ));
    }
    let mut tcx = gossamer_types::TyCtxt::new();
    let (_table, type_diags) = gossamer_types::typecheck_source_file(&sf, &resolutions, &mut tcx);
    if !type_diags.is_empty() {
        for diag in &type_diags {
            let structured = diag.to_diagnostic();
            eprintln!(
                "{}",
                gossamer_diagnostics::render(&structured, &map, render_opts)
            );
        }
        return Err(anyhow!(
            "{} type error(s); refusing to build",
            type_diags.len()
        ));
    }

    // Validate `--target` if explicitly provided. The Cranelift
    // happy-path uses the host ISA; non-host targets fall through
    // to the legacy artifact path (a deterministic byte stream
    // wrapping the rendered module). Reject unknown triples
    // early so the error is a clean parse failure, not a linker
    // blow-up.
    let target_options = match target {
        Some(triple) => Some(
            gossamer_driver::LinkerOptions::for_target(triple)
                .ok_or_else(|| anyhow!("unknown target `{triple}`"))?,
        ),
        None => None,
    };
    let unit_name = default_unit_name(file);
    let out_path = resolve_output_path(file, &unit_name, release)?;

    if let Some(options) = target_options {
        let host = gossamer_driver::TargetTriple::host();
        if options.target.as_str() != host.as_str() {
            let artifact = gossamer_driver::compile_source(&source, &unit_name, &options);
            fs::write(&out_path, &artifact.bytes)
                .map_err(|err| anyhow!("build: writing {}: {err}", out_path.display()))?;
            set_executable(&out_path)?;
            println!(
                "build: {bytes}B artifact at {path} (target {triple}, cross-link pending)",
                bytes = artifact.bytes.len(),
                path = out_path.display(),
                triple = options.target.as_str(),
            );
            return Ok(());
        }
    }
    let opts = LinkOptions {
        release,
        debug_info,
        dynamic,
    };
    let outcome = try_native_build(&source, &unit_name, file, &out_path, opts)
        .map_err(|err| anyhow!("build: {}", err.user_message()))?;
    println!(
        "build: {bytes}B native executable at {path} ({note})",
        bytes = outcome.size,
        path = out_path.display(),
        note = outcome.note,
    );
    Ok(())
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

/// Locates `libgossamer_runtime.a` — the static library produced
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

fn try_native_build(
    source: &str,
    unit_name: &str,
    input_path: &PathBuf,
    out_path: &PathBuf,
    opts: LinkOptions,
) -> std::result::Result<NativeBuildOutcome, NativeBuildError> {
    let tmp_dir =
        std::env::temp_dir().join(format!("gos-build-{}-{}", std::process::id(), unit_name));
    fs::create_dir_all(&tmp_dir)
        .map_err(|err| NativeBuildError::Io(anyhow!("creating {}: {err}", tmp_dir.display())))?;
    let (object_paths, object_triple) =
        emit_native_objects(source, unit_name, &tmp_dir, opts.release)?;
    let runtime_lib = if opts.want_static_musl() {
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
    let bindings_archive = build_static_bindings_lib(opts.release).map_err(|err| {
        NativeBuildError::LinkerMissing(format!("rust-bindings staticlib: {err}"))
    })?;
    let mut extra_archives: Vec<PathBuf> = Vec::new();
    if let Some(p) = bindings_archive {
        extra_archives.push(p);
    }
    let link_result = if cfg!(all(windows, target_env = "msvc")) {
        link_windows_msvc(&object_paths, &runtime_lib, &extra_archives, out_path)
    } else {
        link_posix(&object_paths, &runtime_lib, &extra_archives, out_path, opts)
    };
    let _ = fs::remove_dir_all(&tmp_dir);
    let _ = input_path;
    link_result.map(|()| NativeBuildOutcome {
        size: fs::metadata(out_path).map_or(0, |m| m.len()),
        note: format!(
            "target {triple}{tag}",
            triple = object_triple.as_deref().unwrap_or("unknown"),
            tag = if opts.want_static_musl() {
                ", static-musl"
            } else {
                ""
            },
        ),
    })
}

/// Builds the per-project `libgos_static_bindings.a` if the
/// project declares `[rust-bindings]`. Returns the archive path
/// or `None` when bindings are absent.
fn build_static_bindings_lib(
    release: bool,
) -> std::result::Result<Option<PathBuf>, gossamer_driver::binding_runner::BindingRunnerError> {
    use gossamer_driver::binding_runner::{Profile as RunnerProfile, StaticBindingsLib};
    use gossamer_pkg::{Manifest, find_manifest};

    let Ok(cwd) = std::env::current_dir() else {
        return Ok(None);
    };
    let Some(manifest_path) = find_manifest(&cwd) else {
        return Ok(None);
    };
    let Ok(manifest_text) = fs::read_to_string(&manifest_path) else {
        return Ok(None);
    };
    let Ok(manifest) = Manifest::parse(&manifest_text) else {
        return Ok(None);
    };
    if manifest.rust_bindings.is_empty() {
        return Ok(None);
    }
    let manifest_dir = manifest_path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let Some(gossamer_root) = crate::binding_dispatch::locate_gossamer_root() else {
        return Ok(None);
    };
    let profile = if release {
        RunnerProfile::Release
    } else {
        RunnerProfile::Debug
    };
    let Some(lib) =
        StaticBindingsLib::from_manifest(&manifest, &manifest_dir, &gossamer_root, profile)
            .map_err(gossamer_driver::binding_runner::BindingRunnerError::Io)?
    else {
        return Ok(None);
    };
    let archive = lib.ensure_built()?;
    Ok(Some(archive))
}

/// POSIX/macOS link path. On Linux release builds with the rustup
/// musl target installed and `--dynamic` not set, this routes through
/// `link_posix_static_musl` to produce a fully static binary.
/// Otherwise drives the host `cc` (or `$CC`) for a dynamic-glibc
/// link. macOS always takes the dynamic path (libSystem can't be
/// statically linked, by Apple policy).
fn link_posix(
    object_paths: &[PathBuf],
    runtime_lib: &Path,
    extra_archives: &[PathBuf],
    out_path: &Path,
    opts: LinkOptions,
) -> std::result::Result<(), NativeBuildError> {
    if opts.want_static_musl() {
        return link_posix_static_musl(object_paths, runtime_lib, extra_archives, out_path, opts);
    }

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let mut cmd = std::process::Command::new(&cc);
    for p in object_paths {
        cmd.arg(p);
    }
    cmd.arg(runtime_lib);
    for archive in extra_archives {
        cmd.arg(archive);
    }
    cmd.arg("-o").arg(out_path);
    cmd.arg("-lpthread").arg("-ldl").arg("-lm");
    if !extra_archives.is_empty() {
        // The rust-bindings staticlib pulls in `gossamer-runtime`
        // as a transitive Cargo dep, which produces a second copy
        // of every `gos_rt_*` symbol alongside `libgossamer_runtime.a`.
        // Both copies come from the same source tree and are
        // functionally identical, so let the linker keep the first
        // definition rather than failing the link.
        cmd.arg("-Wl,--allow-multiple-definition");
    }
    if opts.want_strip() {
        // macOS's ld doesn't recognise `--strip-all`; use the
        // dead-strip + post-link `strip` invocation instead.
        if cfg!(target_os = "macos") {
            cmd.arg("-Wl,-dead_strip");
        } else {
            cmd.arg("-Wl,--strip-all").arg("-Wl,--gc-sections");
        }
    }
    match cmd.status() {
        Ok(s) if s.success() => {
            if opts.want_strip() && cfg!(target_os = "macos") {
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

/// Linux static-musl link path — invokes the rustup-shipped `ld.lld`
/// against rustup's self-contained musl CRT/libc/libunwind. Produces
/// a statically-linked ELF that runs on any `x86_64` Linux host
/// regardless of glibc/musl install or version. The cli's build
/// script (`gossamer-cli/build.rs`) builds the runtime against
/// `x86_64-unknown-linux-musl` and bakes the archive path into
/// `MUSL_RUNTIME_LIB` at compile time; here we invoke the linker
/// directly so we don't need `cc` to know about musl.
fn link_posix_static_musl(
    object_paths: &[PathBuf],
    runtime_lib: &Path,
    extra_archives: &[PathBuf],
    out_path: &Path,
    opts: LinkOptions,
) -> std::result::Result<(), NativeBuildError> {
    let sysroot = rustc_sysroot()?;
    let self_contained = sysroot
        .join("lib")
        .join("rustlib")
        .join("x86_64-unknown-linux-musl")
        .join("lib")
        .join("self-contained");
    if !self_contained.exists() {
        return Err(NativeBuildError::LinkerMissing(format!(
            "musl self-contained dir not found: {}; \
             try `rustup target add x86_64-unknown-linux-musl` \
             or pass `--dynamic` to `gos build --release`",
            self_contained.display(),
        )));
    }
    let linker = sysroot
        .join("lib")
        .join("rustlib")
        .join("x86_64-unknown-linux-gnu")
        .join("bin")
        .join("gcc-ld")
        .join("ld.lld");
    if !linker.exists() {
        return Err(NativeBuildError::LinkerMissing(format!(
            "ld.lld not found at {}",
            linker.display(),
        )));
    }

    let mut cmd = std::process::Command::new(&linker);
    cmd.arg("--static")
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
        cmd.arg("--strip-all");
    }
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
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let out = std::process::Command::new(&rustc)
        .args(["--print", "sysroot"])
        .output()
        .map_err(|err| NativeBuildError::LinkerMissing(format!("rustc --print sysroot: {err}")))?;
    if !out.status.success() {
        return Err(NativeBuildError::LinkerMissing(format!(
            "rustc --print sysroot exited with {}",
            out.status
        )));
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// Windows MSVC link path — invokes `rust-lld -flavor link` with
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
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let out = std::process::Command::new(&rustc)
        .args(["--print", "sysroot"])
        .output()
        .map_err(|err| {
            NativeBuildError::LinkerMissing(format!(
                "could not invoke `{rustc} --print sysroot`: {err}"
            ))
        })?;
    if !out.status.success() {
        return Err(NativeBuildError::LinkerMissing(format!(
            "`{rustc} --print sysroot` exited with {}",
            out.status
        )));
    }
    let sysroot = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let candidate = PathBuf::from(&sysroot)
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

/// Lowers `source` into one or two object files under `tmp_dir`,
/// picking the codegen tier from `release`. Returns the object
/// paths plus the recorded target triple for the linker step.
fn emit_native_objects(
    source: &str,
    unit_name: &str,
    tmp_dir: &Path,
    release: bool,
) -> std::result::Result<(Vec<PathBuf>, Option<String>), NativeBuildError> {
    let mut object_paths: Vec<PathBuf> = Vec::new();
    if !release {
        let object = gossamer_driver::compile_source_native(source, unit_name)
            .map_err(|err| NativeBuildError::LowerFailed(err.to_string()))?;
        let object_path = tmp_dir.join(format!("{unit_name}.o"));
        fs::write(&object_path, &object.bytes).map_err(|err| {
            NativeBuildError::Io(anyhow!("writing {}: {err}", object_path.display()))
        })?;
        let triple = Some(object.triple);
        object_paths.push(object_path);
        return Ok((object_paths, triple));
    }
    match gossamer_driver::compile_source_native_release_with_fallback(source, unit_name) {
        Ok(build) => {
            let llvm_path = tmp_dir.join(format!("{unit_name}.llvm.o"));
            fs::write(&llvm_path, &build.llvm.bytes).map_err(|err| {
                NativeBuildError::Io(anyhow!("writing {}: {err}", llvm_path.display()))
            })?;
            let triple = Some(build.llvm.triple.clone());
            object_paths.push(llvm_path);
            if let Some(cl) = build.cranelift {
                let cl_path = tmp_dir.join(format!("{unit_name}.cl.o"));
                fs::write(&cl_path, &cl.bytes).map_err(|err| {
                    NativeBuildError::Io(anyhow!("writing {}: {err}", cl_path.display()))
                })?;
                object_paths.push(cl_path);
                if std::env::var("GOS_LLVM_TRACE").is_ok() {
                    eprintln!(
                        "build: per-function fallback engaged for {n} bodies: {names:?}",
                        n = build.fallback_bodies.len(),
                        names = build.fallback_bodies,
                    );
                }
            }
            Ok((object_paths, triple))
        }
        Err(err) => {
            if std::env::var("GOS_LLVM_TRACE").is_ok() {
                eprintln!(
                    "build: LLVM path rejected `{unit_name}`: {err}; falling back to Cranelift"
                );
            }
            let object = gossamer_driver::compile_source_native(source, unit_name)
                .map_err(|e| NativeBuildError::LowerFailed(e.to_string()))?;
            let object_path = tmp_dir.join(format!("{unit_name}.o"));
            fs::write(&object_path, &object.bytes).map_err(|err| {
                NativeBuildError::Io(anyhow!("writing {}: {err}", object_path.display()))
            })?;
            let triple = Some(object.triple);
            object_paths.push(object_path);
            Ok((object_paths, triple))
        }
    }
}

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
