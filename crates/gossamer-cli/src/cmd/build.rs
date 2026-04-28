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
pub(crate) fn dispatch(path: Option<PathBuf>, target: Option<&str>, release: bool) -> Result<()> {
    let resolved = match path {
        Some(p) => p,
        None => default_main_entry()?,
    };
    run(&resolved, target, release)
}

fn run(file: &PathBuf, target: Option<&str>, release: bool) -> Result<()> {
    let source = read_source(file)?;

    // Validate source before attempting any codegen.  A broken AST or
    // unresolved name must fail the build immediately rather than
    // producing a segfaulting native binary or a launcher that
    // panics at runtime.
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let (sf, parse_diags) = gossamer_parse::parse_source_file(&source, file_id);
    if !parse_diags.is_empty() {
        for diag in &parse_diags {
            eprintln!("{diag}");
        }
        return Err(anyhow!(
            "{} parse error(s); refusing to build",
            parse_diags.len()
        ));
    }
    let (resolutions, resolve_diags) = gossamer_resolve::resolve_source_file(&sf);
    if !resolve_diags.is_empty() {
        for diag in &resolve_diags {
            eprintln!("{diag}");
        }
        return Err(anyhow!(
            "{} resolve error(s); refusing to build",
            resolve_diags.len()
        ));
    }
    let mut tcx = gossamer_types::TyCtxt::new();
    let (_table, type_diags) = gossamer_types::typecheck_source_file(&sf, &resolutions, &mut tcx);
    if !type_diags.is_empty() {
        for diag in &type_diags {
            eprintln!("{diag}");
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
    let outcome = try_native_build(&source, &unit_name, file, &out_path, release)
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
    release: bool,
) -> std::result::Result<NativeBuildOutcome, NativeBuildError> {
    let tmp_dir =
        std::env::temp_dir().join(format!("gos-build-{}-{}", std::process::id(), unit_name));
    fs::create_dir_all(&tmp_dir)
        .map_err(|err| NativeBuildError::Io(anyhow!("creating {}: {err}", tmp_dir.display())))?;
    let (object_paths, object_triple) = emit_native_objects(source, unit_name, &tmp_dir, release)?;
    let runtime_lib = find_runtime_lib()?;
    let link_result = if cfg!(all(windows, target_env = "msvc")) {
        link_windows_msvc(&object_paths, &runtime_lib, out_path)
    } else {
        link_posix(&object_paths, &runtime_lib, out_path)
    };
    let _ = fs::remove_dir_all(&tmp_dir);
    let _ = input_path;
    link_result.map(|()| NativeBuildOutcome {
        size: fs::metadata(out_path).map_or(0, |m| m.len()),
        note: format!(
            "target {triple}",
            triple = object_triple.as_deref().unwrap_or("unknown"),
        ),
    })
}

/// POSIX/macOS link path — drives the host `cc` (or `$CC`).
fn link_posix(
    object_paths: &[PathBuf],
    runtime_lib: &Path,
    out_path: &Path,
) -> std::result::Result<(), NativeBuildError> {
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let mut cmd = std::process::Command::new(&cc);
    for p in object_paths {
        cmd.arg(p);
    }
    cmd.arg(runtime_lib).arg("-o").arg(out_path);
    cmd.arg("-lpthread").arg("-ldl").arg("-lm");
    match cmd.status() {
        Ok(s) if s.success() => {
            set_executable(out_path).map_err(NativeBuildError::Io)?;
            Ok(())
        }
        Ok(s) => Err(NativeBuildError::LinkerFailed(format!(
            "{cc} exited with {s}"
        ))),
        Err(err) => Err(NativeBuildError::LinkerMissing(format!("{cc}: {err}"))),
    }
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
