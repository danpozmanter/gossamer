//! Module-level assembly: runtime symbol declarations +
//! per-function lowering + `llc -O3` invocation.

use std::fmt::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use gossamer_mir::Body;
use gossamer_types::TyCtxt;

use crate::lower::{Lowerer, StringPool};

/// LLVM IR strings that must appear in the module header but are
/// not emitted through `declare_rt()`: LLVM built-in intrinsics,
/// libc `malloc`, the stdout globals, and the three runtime symbols
/// called directly by the C `@main` shim (which is hardcoded in
/// `render_module_inner` rather than lowered from a MIR body).
const LLVM_SPECIAL_DECLS: &[&str] = &[
    "declare ptr @malloc(i64)",
    "declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)",
    "declare void @llvm.lifetime.start.p0(i64, ptr)",
    "declare void @llvm.lifetime.end.p0(i64, ptr)",
    "@GOS_RT_STDOUT_BYTES = external local_unnamed_addr global [8192 x i8]",
    "@GOS_RT_STDOUT_LEN = external local_unnamed_addr global i64",
    // Called directly by the @main shim — not reachable via declare_rt().
    "declare void @gos_rt_set_args(i32, ptr)",
    "declare void @gos_rt_flush_stdout()",
    "declare i32 @gos_rt_main_exit_code(i64)",
];

/// Parallel to `gossamer-codegen-cranelift::NativeObject`.
#[derive(Debug, Clone)]
pub struct NativeObject {
    /// Target triple `llc` was configured for (host by default).
    pub triple: String,
    /// Linker-ready object bytes (ELF / Mach-O depending on host).
    pub bytes: Vec<u8>,
}

/// Reasons the LLVM backend refuses a build. The driver uses
/// `Unsupported` as a signal to fall back to the Cranelift
/// pipeline for programs the MVP doesn't cover.
#[derive(Debug)]
pub enum BuildError {
    /// MIR construct not yet lowered by this backend.
    Unsupported(&'static str),
    /// `llc` not reachable or returned non-zero.
    Tool(String),
    /// IR rendering or temp-file I/O failed.
    Io(anyhow::Error),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(what) => write!(f, "llvm backend: unsupported: {what}"),
            Self::Tool(msg) => write!(f, "llvm backend: tool: {msg}"),
            Self::Io(err) => write!(f, "llvm backend: {err}"),
        }
    }
}

impl std::error::Error for BuildError {}

/// Outcome of a per-function fallback build.
///
/// `object` is the LLVM-emitted object containing every body
/// the lowerer accepted. `fallback_bodies` is the list of body
/// names the lowerer rejected — the driver feeds those into the
/// Cranelift backend, then links the two objects together.
#[derive(Debug, Clone)]
pub struct CompileOutcome {
    /// Object file with the LLVM-lowered bodies.
    pub object: NativeObject,
    /// Names of bodies the LLVM backend declined to lower.
    pub fallback_bodies: Vec<String>,
}

/// Lowers a list of MIR bodies into a native object file via
/// `llc -O3`. The signature mirrors
/// `gossamer-codegen-cranelift::compile_to_object` exactly so
/// the driver can dispatch between the two on the `--release`
/// flag.
pub fn compile_to_object(bodies: &[Body], tcx: &TyCtxt) -> Result<NativeObject> {
    if std::env::var("GOS_LLVM_DUMP_MIR").is_ok() {
        dump_mir(bodies, tcx);
    }
    let triple = host_triple();
    let tmp_dir = pipeline_tmp_dir()?;
    let ll_path = tmp_dir.join("unit.ll");
    let _ = render_module_to_path(bodies, tcx, &ll_path, /*allow_fallback=*/ false)?;
    let obj_path = tmp_dir.join("unit.o");
    invoke_llc_pipeline(&ll_path, &obj_path, &triple)?;
    let bytes =
        std::fs::read(&obj_path).with_context(|| format!("reading {}", obj_path.display()))?;
    let keep_artifacts = std::env::var("GOS_LLVM_DUMP").is_ok();
    if keep_artifacts {
        eprintln!("llvm backend: IR at {}", ll_path.display());
    } else {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
    Ok(NativeObject { triple, bytes })
}

/// Path-oriented variant of [`compile_to_object`]: writes the LLVM
/// object directly to `obj_out` instead of returning bytes. Used
/// by the AOT release driver so the LLVM object never lives in
/// the parent process's heap, only on disk.
pub fn compile_to_object_at_path(
    bodies: &[Body],
    tcx: &TyCtxt,
    obj_out: &std::path::Path,
) -> Result<String> {
    if std::env::var("GOS_LLVM_DUMP_MIR").is_ok() {
        dump_mir(bodies, tcx);
    }
    let triple = host_triple();
    let tmp_dir = pipeline_tmp_dir()?;
    let ll_path = tmp_dir.join("unit.ll");
    let _ = render_module_to_path(bodies, tcx, &ll_path, /*allow_fallback=*/ false)?;
    invoke_llc_pipeline(&ll_path, obj_out, &triple)?;
    let keep_artifacts = std::env::var("GOS_LLVM_DUMP").is_ok();
    if keep_artifacts {
        eprintln!("llvm backend: IR at {}", ll_path.display());
    } else {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
    Ok(triple)
}

fn dump_mir(bodies: &[Body], _tcx: &TyCtxt) {
    for body in bodies {
        eprintln!("=== MIR {} ===", body.name);
        for (i, block) in body.blocks.iter().enumerate() {
            eprintln!("  bb{i}:");
            for stmt in &block.stmts {
                eprintln!("    {:?}", stmt.kind);
            }
            eprintln!("    -> {:?}", block.terminator);
        }
    }
}

/// Per-function fallback build. Each body is attempted
/// individually; bodies the lowerer rejects are returned in
/// `fallback_bodies` so the caller can route them through the
/// Cranelift backend. The LLVM-emitted object includes only the
/// accepted bodies plus an `extern` declaration for each
/// fallback symbol so the linker can resolve them against the
/// Cranelift-built companion object.
pub fn compile_with_fallback(bodies: &[Body], tcx: &TyCtxt) -> Result<CompileOutcome> {
    if std::env::var("GOS_LLVM_DUMP_MIR").is_ok() {
        dump_mir(bodies, tcx);
    }
    let triple = host_triple();
    let tmp_dir = pipeline_tmp_dir()?;
    let ll_path = tmp_dir.join("unit.ll");
    let fallback_bodies =
        render_module_to_path(bodies, tcx, &ll_path, /*allow_fallback=*/ true)?;
    let obj_path = tmp_dir.join("unit.o");
    invoke_llc_pipeline(&ll_path, &obj_path, &triple)?;
    let bytes =
        std::fs::read(&obj_path).with_context(|| format!("reading {}", obj_path.display()))?;
    let keep_artifacts = std::env::var("GOS_LLVM_DUMP").is_ok();
    if keep_artifacts {
        eprintln!("llvm backend: IR at {}", ll_path.display());
    } else {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
    Ok(CompileOutcome {
        object: NativeObject { triple, bytes },
        fallback_bodies,
    })
}

/// Path-oriented variant of [`compile_with_fallback`]: writes the
/// LLVM object directly to `obj_out` and returns only the triple
/// plus the per-function fallback list. The object never lives
/// in the parent process's heap.
pub fn compile_with_fallback_at_path(
    bodies: &[Body],
    tcx: &TyCtxt,
    obj_out: &std::path::Path,
) -> Result<(String, Vec<String>)> {
    if std::env::var("GOS_LLVM_DUMP_MIR").is_ok() {
        dump_mir(bodies, tcx);
    }
    let triple = host_triple();
    let tmp_dir = pipeline_tmp_dir()?;
    let ll_path = tmp_dir.join("unit.ll");
    let fallback_bodies =
        render_module_to_path(bodies, tcx, &ll_path, /*allow_fallback=*/ true)?;
    invoke_llc_pipeline(&ll_path, obj_out, &triple)?;
    let keep_artifacts = std::env::var("GOS_LLVM_DUMP").is_ok();
    if keep_artifacts {
        eprintln!("llvm backend: IR at {}", ll_path.display());
    } else {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
    Ok((triple, fallback_bodies))
}

/// Streaming renderer: writes the full module to `ll_path` without
/// retaining a complete IR `String` in memory. Bodies are emitted
/// directly to a temp body file as they're lowered, then spliced
/// into the final IR file behind the header / globals / pool.
///
/// Returns the names of bodies that fell back to Cranelift when
/// `allow_fallback` is true. Bodies that emit an LLVM-internal
/// tool error always abort regardless of `allow_fallback`.
fn render_module_to_path(
    bodies: &[Body],
    tcx: &TyCtxt,
    ll_path: &std::path::Path,
    allow_fallback: bool,
) -> Result<Vec<String>> {
    use std::io::{BufWriter, Write as _};

    if let Some(parent) = ll_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let body_path = ll_path.with_file_name(match ll_path.file_name() {
        Some(name) => format!("{}.body", name.to_string_lossy()),
        None => "module.body".to_string(),
    });

    let mut fn_name_by_def: std::collections::HashMap<u32, String> =
        std::collections::HashMap::new();
    let mut param_tys_by_name: std::collections::HashMap<String, Vec<gossamer_types::Ty>> =
        std::collections::HashMap::new();
    for body in bodies {
        if let Some(def) = body.def {
            fn_name_by_def.insert(def.local, body.name.clone());
        }
        // Per-callee param-type table: `emit_named_call` consults
        // this to pass `&Adt` arguments as the heap pointer
        // (loaded from the slot) rather than the slot address.
        // Without it, `length(&xs)` receives the slot's address
        // and the disc read at offset 0 misses the heap blob.
        let param_tys: Vec<gossamer_types::Ty> = (0..body.arity)
            .map(|i| body.local_ty(gossamer_mir::Local(i + 1)))
            .collect();
        param_tys_by_name.insert(body.name.clone(), param_tys);
    }

    let mut globals: Vec<String> = Vec::new();
    let mut fallback_bodies: Vec<String> = Vec::new();
    let string_pool = std::rc::Rc::new(std::cell::RefCell::new(StringPool::default()));

    // Inter-procedural capture summary: feeds the cleanup pass so
    // owning bindings whose only outbound use is a non-capturing
    // user fn can get a precise per-block drop instead of being
    // forced into the escape set.
    let capture_summary = gossamer_mir::build_capture_summary(bodies);

    let body_file = std::fs::File::create(&body_path)
        .with_context(|| format!("creating {}", body_path.display()))?;
    let mut body_w = BufWriter::with_capacity(64 * 1024, body_file);

    for body in bodies {
        let mut lowerer = Lowerer::new(body, tcx);
        lowerer.fn_name_by_def.clone_from(&fn_name_by_def);
        lowerer.param_tys_by_name.clone_from(&param_tys_by_name);
        lowerer.strings = string_pool.clone();
        lowerer.capture_summary = capture_summary.clone();
        match lowerer.lower() {
            Ok(text) => {
                body_w
                    .write_all(text.as_bytes())
                    .with_context(|| format!("writing {}", body_path.display()))?;
                body_w
                    .write_all(b"\n")
                    .with_context(|| format!("writing {}", body_path.display()))?;
                globals.extend(lowerer.take_module_globals());
            }
            Err(BuildError::Unsupported(msg)) => {
                // `GOSSAMER_FAIL_ON_LLVM_FALLBACK=1` turns the
                // silent per-fn Cranelift fallback into a hard
                // error. Used in CI to gate "must stay on the
                // LLVM backend" programs against silent
                // regressions like the 2026-04-28 / 2026-04-30
                // spectral-norm slowdowns where a malformed
                // `runtime_refs` entry kicked the body off LLVM
                // without surfacing in any human-readable signal.
                if want_strict_lowering() {
                    let _ = std::fs::remove_file(&body_path);
                    return Err(anyhow!(
                        "llvm backend: `{fn_name}` would fall back to Cranelift ({msg}) but \
                         strict-lowering is enabled (set_strict_lowering(true) or \
                         GOSSAMER_FAIL_ON_LLVM_FALLBACK=1)",
                        fn_name = body.name,
                    ));
                }
                if allow_fallback {
                    if std::env::var("GOS_LLVM_TRACE").is_ok() {
                        eprintln!(
                            "llvm backend: routing `{name}` to Cranelift fallback ({msg})",
                            name = body.name,
                        );
                    }
                    fallback_bodies.push(body.name.clone());
                    let decl = extern_declare(body, tcx);
                    body_w
                        .write_all(decl.as_bytes())
                        .with_context(|| format!("writing {}", body_path.display()))?;
                    body_w
                        .write_all(b"\n")
                        .with_context(|| format!("writing {}", body_path.display()))?;
                } else {
                    let _ = std::fs::remove_file(&body_path);
                    return Err(anyhow!(
                        "llvm backend: cannot lower `{fn_name}`: {msg}",
                        fn_name = body.name,
                    ));
                }
            }
            Err(BuildError::Tool(msg)) => {
                let _ = std::fs::remove_file(&body_path);
                return Err(anyhow!("llvm backend: tool: {msg}"));
            }
            Err(BuildError::Io(err)) => {
                let _ = std::fs::remove_file(&body_path);
                return Err(err);
            }
        }
    }

    let mut thunk_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for body in bodies {
        collect_thunk_names_in_body(body, &mut thunk_names);
    }
    for name in &thunk_names {
        if let Some(text) = render_shape_thunk(name) {
            body_w
                .write_all(text.as_bytes())
                .with_context(|| format!("writing {}", body_path.display()))?;
            body_w
                .write_all(b"\n")
                .with_context(|| format!("writing {}", body_path.display()))?;
        }
    }

    if let Some(user_main) = bodies.iter().find(|b| b.name == "main") {
        let ret_is_unit = matches!(
            tcx.kind(user_main.local_ty(gossamer_mir::Local::RETURN)),
            Some(gossamer_types::TyKind::Unit)
        );
        writeln!(body_w, "define i32 @main(i32 %argc, ptr %argv) {{")?;
        writeln!(body_w, "entry:")?;
        writeln!(body_w, "  call void @gos_rt_set_args(i32 %argc, ptr %argv)")?;
        if ret_is_unit {
            writeln!(body_w, "  call void @\"gos_main\"()")?;
            writeln!(body_w, "  call void @gos_rt_flush_stdout()")?;
            writeln!(body_w, "  ret i32 0")?;
        } else {
            writeln!(body_w, "  %r = call i64 @\"gos_main\"()")?;
            writeln!(body_w, "  call void @gos_rt_flush_stdout()")?;
            writeln!(body_w, "  %code = call i32 @gos_rt_main_exit_code(i64 %r)")?;
            writeln!(body_w, "  ret i32 %code")?;
        }
        writeln!(body_w, "}}")?;
    }
    writeln!(body_w)?;
    writeln!(body_w, "!0 = !{{}}")?;

    body_w
        .flush()
        .with_context(|| format!("flushing {}", body_path.display()))?;
    drop(body_w);

    // Now write the final IR file. Header → special decls →
    // sorted/deduped globals → string pool → body file content.
    globals.sort();
    globals.dedup();
    let ll_file = std::fs::File::create(ll_path)
        .with_context(|| format!("creating {}", ll_path.display()))?;
    let mut ll_w = BufWriter::with_capacity(64 * 1024, ll_file);
    writeln!(ll_w, "; ModuleID = \"gossamer\"")?;
    writeln!(ll_w, "target triple = \"{}\"", host_triple())?;
    if want_reproducible() {
        writeln!(ll_w, "; reproducible-build = true")?;
    }
    writeln!(ll_w)?;
    for d in LLVM_SPECIAL_DECLS {
        writeln!(ll_w, "{d}")?;
    }
    writeln!(ll_w)?;
    // Shape-validate each accumulated global. The `runtime_refs`
    // BTreeSet inside `Lowerer` accepts arbitrary strings; a
    // malformed entry corrupts the IR string and silently flips
    // affected bodies to the Cranelift fallback. Each entry must
    // be either an `@symbol = ...` definition or a `declare ...`
    // function declaration.
    // Dedupe declarations by symbol name: each lowerer body
    // accumulates its own `declare` lines, but two bodies that call
    // the same runtime helper with ABI-compatible-but-different
    // operand types (e.g. `gos_rt_result_new(i64, i64)` vs
    // `(i64, ptr)`) would each emit a `declare` and LLVM rejects
    // the redefinition. Pick the first declaration we see for a
    // given symbol; the calls themselves are individually typed and
    // the ABI tolerates the i64/ptr substitution on x86_64.
    let mut emitted_decls: std::collections::HashSet<String> = std::collections::HashSet::new();
    for g in &globals {
        validate_global_decl_shape(g)?;
        if let Some(rest) = g.strip_prefix("declare ") {
            // Parse "<ret> @<name>(...)" — name is the substring
            // between '@' and '('.
            if let Some(at_idx) = rest.find('@')
                && let Some(open_idx) = rest[at_idx..].find('(')
            {
                let symbol = &rest[at_idx + 1..at_idx + open_idx];
                let symbol = symbol.trim_matches('"');
                if !emitted_decls.insert(symbol.to_string()) {
                    continue;
                }
            }
        }
        writeln!(ll_w, "{g}")?;
    }
    if !globals.is_empty() {
        writeln!(ll_w)?;
    }
    let pool_text = string_pool.borrow().render();
    if !pool_text.is_empty() {
        ll_w.write_all(pool_text.as_bytes())?;
        writeln!(ll_w)?;
    }
    let mut body_in = std::fs::File::open(&body_path)
        .with_context(|| format!("opening {}", body_path.display()))?;
    std::io::copy(&mut body_in, &mut ll_w)
        .with_context(|| format!("appending body buffer to {}", ll_path.display()))?;
    drop(body_in);
    let _ = std::fs::remove_file(&body_path);
    ll_w.flush()
        .with_context(|| format!("flushing {}", ll_path.display()))?;
    drop(ll_w);

    // DWARF emission needs to insert `!dbg !N` after each function
    // header. Defer to the in-memory string mutator on the rare `-g`
    // path; the streaming default never pays this cost.
    if want_dwarf() {
        let mut content = std::fs::read_to_string(ll_path)
            .with_context(|| format!("reading {}", ll_path.display()))?;
        emit_dwarf_metadata(&mut content, bodies);
        std::fs::write(ll_path, &content)
            .with_context(|| format!("writing {}", ll_path.display()))?;
    }

    Ok(fallback_bodies)
}

/// Process-wide flag toggled by [`set_debug_info`] so the CLI can
/// request DWARF emission without going through an env var (which
/// would require `unsafe` to set on stable Rust 2024).
static DEBUG_INFO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Process-wide flag toggled by [`set_reproducible`] requesting
/// bit-identical builds across runs. Sets `SOURCE_DATE_EPOCH`
/// (read by `llc`), strips embedded paths from the IR module
/// header, and forces a sorted symbol table on the output.
static REPRODUCIBLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Process-wide flag toggled by [`set_strict_lowering`] requesting
/// that any `BuildError::Unsupported` produce a top-level error
/// rather than the historical per-function Cranelift fallback.
/// `gos build` sets this for itself so the CLI never silently
/// links Cranelift-emitted bodies into a release binary — under
/// the canonical-LLVM policy, Cranelift is the JIT-only backend
/// and `gos build` is LLVM-only. The pre-existing
/// `GOSSAMER_FAIL_ON_LLVM_FALLBACK` env var continues to enable
/// the same behaviour for callers that prefer env-driven config
/// (tier_parity tests already use it).
static STRICT_LOWERING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Process-wide flag toggled by [`set_race_instrumentation`]. When on,
/// the LLVM emitter wraps every `gos_load` / `gos_store` raw-heap
/// intrinsic with a `gos_rt_race_access(addr, write)` call so the
/// runtime detector can observe the access. Off by default; the CLI
/// flips it for `gos test --race` / `gos build --race`.
static RACE_INSTRUMENTATION: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Process-wide optimisation-profile flag toggled by
/// [`set_opt_profile`]. `0` = release (full `opt -O3 | llc -O3`
/// pipeline); `1` = debug (skip the `opt` pre-pass, run `llc -O0`).
/// Default is release so callers that don't configure the profile
/// see the historical behaviour. `gos build` flips this to debug
/// when the user omits `--release`.
static OPT_PROFILE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Enables (or disables) DWARF emission for subsequent
/// [`compile_to_object`] / [`compile_with_fallback`] calls.
/// Called by the `gos build --release -g` flag.
pub fn set_debug_info(enabled: bool) {
    DEBUG_INFO.store(enabled, std::sync::atomic::Ordering::Release);
}

/// Enables (or disables) reproducible-build mode. Used by
/// `gos build --reproducible`.
pub fn set_reproducible(enabled: bool) {
    REPRODUCIBLE.store(enabled, std::sync::atomic::Ordering::Release);
}

/// `true` when reproducible-build mode is on.
fn want_reproducible() -> bool {
    REPRODUCIBLE.load(std::sync::atomic::Ordering::Acquire)
}

/// Enables (or disables) strict-lowering mode. When on, the LLVM
/// emitter treats any `BuildError::Unsupported` as a top-level
/// error and refuses to fall back to Cranelift per-function. The
/// `gos build` CLI sets this true for itself so the canonical-
/// LLVM policy holds without touching env vars (the workspace
/// forbids `unsafe_code` in the CLI crate, so `std::env::set_var`
/// is unavailable there).
pub fn set_strict_lowering(enabled: bool) {
    STRICT_LOWERING.store(enabled, std::sync::atomic::Ordering::Release);
}

/// Enables (or disables) race-detector instrumentation for
/// subsequent emits. When on, the LLVM lowerer wraps every
/// `gos_load` / `gos_store` raw-heap intrinsic with a
/// `gos_rt_race_access(addr, write)` call so the runtime
/// detector observes the access. `gos test --race` /
/// `gos build --race` flip this on.
pub fn set_race_instrumentation(enabled: bool) {
    RACE_INSTRUMENTATION.store(enabled, std::sync::atomic::Ordering::Release);
}

/// `true` when race-detector instrumentation is requested.
#[must_use]
pub fn want_race_instrumentation() -> bool {
    RACE_INSTRUMENTATION.load(std::sync::atomic::Ordering::Acquire)
}

/// `true` when strict lowering is requested — either by
/// [`set_strict_lowering`] or by the legacy
/// `GOSSAMER_FAIL_ON_LLVM_FALLBACK` env var.
fn want_strict_lowering() -> bool {
    if STRICT_LOWERING.load(std::sync::atomic::Ordering::Acquire) {
        return true;
    }
    std::env::var("GOSSAMER_FAIL_ON_LLVM_FALLBACK")
        .ok()
        .is_some_and(|v| !v.is_empty() && v != "0")
}

/// Optimisation profile selector for [`set_opt_profile`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptProfile {
    /// Release: full `opt -O3 | llc -O3` pipeline. Default.
    Release,
    /// Debug: skip the `opt` pre-pass, run `llc -O0`. Faster
    /// compile, no mid-level optimisation, debug-friendly IR
    /// shapes preserved.
    Debug,
}

/// Sets the optimisation profile for subsequent emits. `gos build`
/// flips this to `Debug` when the user omits `--release`.
pub fn set_opt_profile(profile: OptProfile) {
    let v: u8 = match profile {
        OptProfile::Release => 0,
        OptProfile::Debug => 1,
    };
    OPT_PROFILE.store(v, std::sync::atomic::Ordering::Release);
}

/// Reads the active optimisation profile.
fn opt_profile() -> OptProfile {
    match OPT_PROFILE.load(std::sync::atomic::Ordering::Acquire) {
        1 => OptProfile::Debug,
        _ => OptProfile::Release,
    }
}

/// `true` when the build should embed DWARF debug information.
/// Triggered by either the `GOS_DWARF` env var (used by tests),
/// the `GOS_BUILD_DEBUG` env var (CI), or [`set_debug_info`] (CLI
/// `-g` flag).
fn want_dwarf() -> bool {
    DEBUG_INFO.load(std::sync::atomic::Ordering::Acquire)
        || std::env::var("GOS_DWARF").is_ok()
        || std::env::var("GOS_BUILD_DEBUG").is_ok()
}

/// Emits LLVM debug-info metadata for every body in `bodies`.
/// Produces:
///
/// - `llvm.module.flags` declaring DWARF v4 and Debug Info v3.
/// - One [`DICompileUnit`] for the program, owning a single
///   synthetic [`DIFile`] (the source map is not yet plumbed
///   through to the lowerer; per-function file resolution is a
///   follow-up).
/// - One [`DISubprogram`] per body, attached to the function's
///   `define` line via `!dbg !N`. The subprogram metadata is what
///   `gdb` / `lldb` use to walk a backtrace and resolve
///   instruction pointers to function names.
fn emit_dwarf_metadata(out: &mut String, bodies: &[Body]) {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| ".".to_string());
    // 1. Tag the function definitions with `!dbg !N`. The
    //    subprogram numbers start at 100; the file is !50, the
    //    compile unit is !51.
    let mut subprogram_lines: Vec<String> = Vec::new();
    for (idx, body) in bodies.iter().enumerate() {
        let llvm_name = if body.name == "main" {
            "gos_main"
        } else {
            body.name.as_str()
        };
        let id = 100u32 + u32::try_from(idx).unwrap_or(u32::MAX);
        // Best-effort: stamp every function with the body name and
        // a stable scopeLine of 1. Real source line numbers will
        // arrive once the SourceMap is threaded through the
        // codegen pipeline.
        subprogram_lines.push(format!(
            "!{id} = distinct !DISubprogram(name: \"{name}\", linkageName: \"{lname}\", \
             scope: !51, file: !50, line: 1, type: !52, scopeLine: 1, \
             spFlags: DISPFlagDefinition, unit: !51)",
            id = id,
            name = body.name.replace('"', "\\\""),
            lname = llvm_name.replace('"', "\\\""),
        ));
        // Attach `!dbg` to the define line.
        let needle = format!("define i64 @\"{llvm_name}\"");
        let attached = format!("define i64 @\"{llvm_name}\"");
        if let Some(pos) = out.find(&needle) {
            // Scan forward to the opening brace and insert `!dbg !N`
            // just before it.
            if let Some(brace) = out[pos..].find(" {\n") {
                let abs = pos + brace;
                let insertion = format!(" !dbg !{id}");
                out.insert_str(abs, &insertion);
                continue;
            }
            let _ = attached;
        }
        // Same scan for the `void`-returning shape.
        let needle_void = format!("define void @\"{llvm_name}\"");
        if let Some(pos) = out.find(&needle_void) {
            if let Some(brace) = out[pos..].find(" {\n") {
                let abs = pos + brace;
                let insertion = format!(" !dbg !{id}");
                out.insert_str(abs, &insertion);
            }
        }
    }
    writeln!(out).unwrap();
    writeln!(out, "!llvm.module.flags = !{{!40, !41}}").unwrap();
    writeln!(out, "!llvm.dbg.cu = !{{!51}}").unwrap();
    writeln!(out, "!40 = !{{i32 7, !\"Dwarf Version\", i32 4}}").unwrap();
    writeln!(out, "!41 = !{{i32 2, !\"Debug Info Version\", i32 3}}").unwrap();
    writeln!(
        out,
        "!50 = !DIFile(filename: \"main.gos\", directory: \"{dir}\")",
        dir = cwd.replace('"', "\\\""),
    )
    .unwrap();
    writeln!(
        out,
        "!51 = distinct !DICompileUnit(language: DW_LANG_C99, file: !50, \
         producer: \"gossamer 0.0.0\", isOptimized: true, runtimeVersion: 0, \
         emissionKind: FullDebug)"
    )
    .unwrap();
    writeln!(out, "!52 = !DISubroutineType(types: !{{}})").unwrap();
    for line in subprogram_lines {
        writeln!(out, "{line}").unwrap();
    }
}

/// Renders an `extern declare` for a body LLVM is offloading
/// to the Cranelift fallback. The signature must match what
/// the Cranelift backend will emit for the same MIR body so the
/// linker can hook them up.
/// Verifies a single module-level global declaration string has
/// the structural shape LLVM IR expects. We don't parse the full
/// grammar — we only check the prefix tokens an entry must lead
/// with. The check is cheap (string scan, no allocation) and
/// catches the realistic regression mode: a *bare* identifier
/// (e.g. `"my_const"` instead of `"@my_const = constant ..."`)
/// being inserted via `runtime_refs.insert(...)`. That class of
/// bug previously corrupted the IR module silently and forced
/// `llc` to error which then triggered the per-fn Cranelift
/// fallback for unrelated bodies.
/// Walks `body`'s MIR statements + terminators looking for
/// `gos_fn_addr("__fn_thunk_*")` references. The names matter
/// because each unique shape needs a synthesised LLVM thunk;
/// see [`render_shape_thunk`].
fn collect_thunk_names_in_body(body: &Body, out: &mut std::collections::BTreeSet<String>) {
    use gossamer_mir::{ConstValue, Operand, Rvalue, StatementKind, Terminator};
    let mut visit_args = |args: &[Operand], name: &str| {
        if name == "gos_fn_addr"
            && let Some(Operand::Const(ConstValue::Str(s))) = args.first()
            && s.starts_with("__fn_thunk_")
        {
            out.insert(s.clone());
        }
    };
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign { rvalue, .. } = &stmt.kind
                && let Rvalue::CallIntrinsic { name, args } = rvalue
            {
                visit_args(args, name);
            }
        }
        if let Terminator::Call { callee, args, .. } = &block.terminator
            && let Operand::Const(ConstValue::Str(name)) = callee
        {
            visit_args(args, name);
        }
    }
}

/// Synthesises an LLVM `define` for a per-shape callable thunk
/// named `__fn_thunk_<inputs>_<ret>`. The thunk loads the real
/// fn pointer from `env+8` and forwards the typed arguments
/// with the matching calling convention. Mirrors the Cranelift
/// backend's `define_shape_thunk` so capturing closures and
/// fn-item refs flow through identical lowering.
fn render_shape_thunk(name: &str) -> Option<String> {
    let suffix = name.strip_prefix("__fn_thunk_")?;
    let (inputs_str, ret_str) = suffix.rsplit_once('_')?;
    let ret_char = ret_str.chars().next()?;
    let ret_ty = shape_char_to_llvm_ty(ret_char)?;
    let mut input_tys: Vec<&'static str> = Vec::with_capacity(inputs_str.len());
    for c in inputs_str.chars() {
        input_tys.push(shape_char_to_llvm_ty(c)?);
    }
    let unit_ret = ret_char == 'u';
    let mut out = String::new();
    let header_ret = if unit_ret { "void" } else { ret_ty };
    let mut params = String::from("ptr %env");
    for (i, t) in input_tys.iter().enumerate() {
        let _ = write!(params, ", {t} %a{i}");
    }
    let _ = writeln!(out, "define {header_ret} @\"{name}\"({params}) {{");
    writeln!(out, "entry:").unwrap();
    writeln!(out, "  %fn_ptr_addr = getelementptr i8, ptr %env, i64 8").unwrap();
    writeln!(out, "  %fn_ptr = load ptr, ptr %fn_ptr_addr").unwrap();
    let mut call_args = String::new();
    for (i, t) in input_tys.iter().enumerate() {
        if i > 0 {
            call_args.push_str(", ");
        }
        let _ = write!(call_args, "{t} %a{i}");
    }
    if unit_ret {
        let _ = writeln!(out, "  call void %fn_ptr({call_args})");
        writeln!(out, "  ret void").unwrap();
    } else {
        let _ = writeln!(out, "  %r = call {ret_ty} %fn_ptr({call_args})");
        let _ = writeln!(out, "  ret {ret_ty} %r");
    }
    writeln!(out, "}}").unwrap();
    Some(out)
}

/// Maps a shape character produced by
/// `gossamer_mir::mangle_callable_shape` to its LLVM IR type
/// name. Mirrors `shape_char_to_cl_type` on the Cranelift side.
fn shape_char_to_llvm_ty(c: char) -> Option<&'static str> {
    Some(match c {
        'b' | 'y' => "i8",
        'k' => "i16",
        'c' | 'j' => "i32",
        'i' => "i64",
        'f' => "double",
        'g' => "float",
        'u' => "i64",
        _ => return None,
    })
}

fn validate_global_decl_shape(g: &str) -> Result<()> {
    let trimmed = g.trim_start();
    let valid = trimmed.starts_with('@') || trimmed.starts_with("declare ");
    if !valid {
        return Err(anyhow!(
            "llvm backend: malformed module-level entry (expected `@symbol = ...` or \
             `declare ...`, got: {snippet:?}). This is the same shape regression that \
             caused the 2026-04-28 / 2026-04-30 silent Cranelift-fallback incidents.",
            snippet = if trimmed.len() > 80 {
                &trimmed[..80]
            } else {
                trimmed
            }
        ));
    }
    Ok(())
}

fn extern_declare(body: &Body, tcx: &TyCtxt) -> String {
    let ret_ty = crate::ty::render_ty(tcx, body.local_ty(gossamer_mir::Local::RETURN));
    let mut params = String::new();
    for i in 0..body.arity {
        if i > 0 {
            params.push_str(", ");
        }
        let local = gossamer_mir::Local(i + 1);
        let p_ty = crate::ty::render_ty(tcx, body.local_ty(local));
        let _ = write!(params, "{p_ty}");
    }
    format!(
        "declare {ret_ty} @\"{name}\"({params})\n",
        name = crate::lower::mangle_fn_name(&body.name)
    )
}

/// Returns the temp directory the LLVM pipeline emits its
/// intermediate IR / opt-bitcode artifacts into. Reproducible mode
/// pins a stable name; otherwise the directory is namespaced with
/// the process id so concurrent builds don't clobber each other.
fn pipeline_tmp_dir() -> Result<PathBuf> {
    let tmp_dir = if want_reproducible() {
        std::env::temp_dir().join("gos-llvm-reproducible")
    } else {
        std::env::temp_dir().join(format!("gos-llvm-{}", std::process::id()))
    };
    std::fs::create_dir_all(&tmp_dir).with_context(|| format!("creating {}", tmp_dir.display()))?;
    Ok(tmp_dir)
}

/// Path-only variant of the historical `invoke_llc(ir_str, triple)
/// -> Vec<u8>`. Reads the IR from `ll_path` (already on disk) and
/// writes the resulting object directly to `obj_out`. The previous
/// API forced callers to round-trip the IR + the object through
/// memory; this one keeps both on disk and returns nothing.
fn invoke_llc_pipeline(
    ll_path: &std::path::Path,
    obj_out: &std::path::Path,
    triple: &str,
) -> Result<()> {
    // Reproducible / scratch siblings of `ll_path`. `unit.opt.bc`
    // is the post-`opt` bitcode; `obj_out` is the caller-chosen
    // final object (kept across invocations only when GOS_LLVM_DUMP
    // is set). Locating the bitcode next to the IR keeps the
    // pipeline self-contained for `llc` invocation.
    let opt_path = ll_path.with_extension("opt.bc");
    let keep_artifacts = std::env::var("GOS_LLVM_DUMP").is_ok();
    if keep_artifacts {
        // Emit the canonical dump-path marker the
        // `llvm_lowering_marker` test (and ad-hoc debug runs)
        // grep for. Pinning the line shape lets tooling locate
        // the IR file without guessing a temp-dir layout.
        eprintln!("llvm backend: IR at {}", ll_path.display());
    }
    let profile = opt_profile();
    let mcpu = mcpu_target();
    // Both profiles run `opt` because the lowerer emits some
    // non-canonical shapes (e.g. integer-typed constants in
    // floating-point store positions) that `opt`'s
    // instcombine + verifier passes fix up. Skipping `opt`
    // entirely sends those shapes straight to `llc`, which
    // rejects them. Debug profile runs `opt -O1` (faster, no
    // vectoriser / loop unroller) and `llc -O0`; release
    // profile runs `opt -O3 | llc -O3`.
    let (opt_level, llc_level) = match profile {
        OptProfile::Debug => ("-O1", "-O0"),
        OptProfile::Release => ("-O3", "-O3"),
    };
    let opt_tool = find_opt()?;
    let mut opt_cmd = std::process::Command::new(&opt_tool);
    opt_cmd
        .arg(opt_level)
        .arg(format!("-mtriple={triple}"))
        // Match `rustc -C target-cpu=native`: tell the
        // mid-level optimiser the target's feature set so the
        // loop / SLP vectorisers can emit AVX2 / FMA when the
        // host supports them. Without this, `opt` only knows
        // the baseline triple's features.
        //
        // `GOS_LLVM_MCPU` overrides — `x86-64-v3` is the
        // documented escape hatch when the host's AVX-512
        // entry/exit transition penalty hurts short-running
        // benchmarks (the §5 release-perf investigation
        // found this on fannkuch).
        .arg(format!("-mcpu={mcpu}"))
        // Cap vectoriser width at 256 bits. Without this,
        // LLVM-O3 + `-mcpu=native` on AVX-512 hosts (Zen 5,
        // Sapphire Rapids, etc.) eagerly widens hot inner loops
        // to ZMM, then has to save/restore them around runtime
        // calls (`gos_rt_*`) — costing more than it saves on
        // small-trip-count loops like fannkuch's `perm.swap`.
        // YMM (256-bit) is the sweet spot: AVX2 and FMA still
        // fire on workloads that genuinely benefit (nbody,
        // spectral-norm), but the ZMM dirty-state churn around
        // runtime calls disappears. Matches the upstream
        // recommendation for AVX-512 codegen on cores where
        // 512-bit ops down-clock or share execution-port budget
        // with scalar work.
        .arg("-mattr=+prefer-256-bit")
        // Block `LoopIdiomRecognize` from rewriting trivial
        // copy / shift loops into `llvm.memcpy` / `llvm.memmove`
        // calls. Once a memcpy/memmove appears with a runtime
        // size, `llc` has no choice but to emit a libc PLT call
        // (musl's `memcpy`), and on small `n` (< ~16) the call
        // overhead — argument setup, PLT trampoline, and YMM
        // save/restore around it — dwarfs the actual work, so
        // the "compiled" Cranelift tier (which inlines the loop
        // verbatim) ends up faster than `--release` LLVM-O3.
        // Keeping idiom-recognise off matches the inline-loop
        // shape that beats Cranelift on fannkuch, and leaves
        // genuinely large copies (compiler-emitted aggregate
        // moves via explicit `llvm.memcpy` intrinsics) untouched
        // because those go through a different lowering path
        // that this flag does not gate.
        //
        // The narrower `disable-memcpy-idiom` /
        // `disable-memmove-idiom` flags exist but no longer take
        // effect under LLVM 18's new pass manager — see the §5
        // release-perf investigation in the bench-game audit.
        .arg("--disable-loop-idiom-all");
    // PGO instrumentation mode: `GOS_PGO_COLLECT=<output.profraw>`
    // builds an instrumented binary that emits raw profile data when
    // the program exits. Link with `libclang_rt.profile-x86_64.a`
    // (handled in `gossamer-cli/src/cmd/build.rs`); merge the
    // resulting `.profraw` with `llvm-profdata merge -output=...`.
    if let Ok(profraw) = std::env::var("GOS_PGO_COLLECT") {
        opt_cmd
            .arg("--pgo-kind=pgo-instr-gen-pipeline")
            .arg(format!("--pgo-test-profile-file={profraw}"));
    }
    // PGO optimisation mode: `GOS_PGO_PROFILE=<merged.profdata>`
    // feeds a previously collected and merged profile into the `opt`
    // mid-end so branch weights, inlining thresholds, and the loop /
    // SLP vectorisers are guided by real execution frequencies.
    // Typical speedup: 5–10% on compute-heavy workloads. The two
    // modes are mutually exclusive; setting both is undefined.
    if let Ok(profdata) = std::env::var("GOS_PGO_PROFILE") {
        opt_cmd
            .arg("--pgo-kind=pgo-instr-use-pipeline")
            .arg(format!("--profile-file={profdata}"));
    }
    opt_cmd.arg(ll_path).arg("-o").arg(&opt_path);
    let opt_output = run_with_timeout(opt_cmd, opt_timeout(), "opt")
        .with_context(|| format!("spawn {}", opt_tool.display()))?;
    if !opt_output.status.success() {
        if keep_artifacts {
            eprintln!("llvm backend: failing IR kept at {}", ll_path.display());
        }
        return Err(anyhow!(
            "opt failed ({status}): {stderr}\n\
             hint: largest IR usually drives `opt -O3` blowups; \
             dump with GOS_LLVM_DUMP=1 and inspect the function \
             names in the IR to find the offender.",
            status = opt_output.status,
            stderr = String::from_utf8_lossy(&opt_output.stderr)
        ));
    }
    // Backend: `llc -O3` → object file with PIC relocations
    // (matches the rest of the build pipeline; the linker
    // refuses non-PIC objects for default PIE binaries).
    // `-mcpu=native` lets LLVM target the host's full
    // instruction set (AVX2 / FMA / etc. on modern Ryzen) —
    // matches what `rustc -C target-cpu=native` does for the
    // bench-game references.
    let llc = find_llc()?;
    let mut llc_cmd = std::process::Command::new(&llc);
    llc_cmd
        .arg(llc_level)
        .arg("-filetype=obj")
        .arg(format!("-mtriple={triple}"))
        .arg("-relocation-model=pic")
        .arg(format!("-mcpu={mcpu}", mcpu = mcpu_target()))
        // See the matching note on the `opt` invocation: cap
        // the late-stage vectoriser at 256-bit too so any
        // remaining post-`opt` codegen (slow-path lowering,
        // memcpy/memset expansion) doesn't reach for ZMM.
        .arg("-mattr=+prefer-256-bit")
        .arg(&opt_path)
        .arg("-o")
        .arg(obj_out);
    // Pin DWARF version to match what the module metadata declared
    // (`!{i32 7, "Dwarf Version", i32 4}`). `llc` may otherwise pick
    // a newer default if the host LLVM is bumped, producing object
    // files that older debuggers can't read.
    if want_dwarf() {
        llc_cmd.arg("-dwarf-version=4");
    }
    let output = run_with_timeout(llc_cmd, opt_timeout(), "llc")
        .with_context(|| format!("spawn {}", llc.display()))?;
    if !output.status.success() {
        if keep_artifacts {
            eprintln!("llvm backend: failing IR kept at {}", ll_path.display());
        }
        return Err(anyhow!(
            "llc failed ({status}): {stderr}",
            status = output.status,
            stderr = String::from_utf8_lossy(&output.stderr)
        ));
    }
    let _ = std::fs::remove_file(&opt_path);
    Ok(())
}

/// Returns the wall-clock cap for the `opt` and `llc` subprocesses.
/// `GOS_LLVM_OPT_TIMEOUT_SECS=N` overrides; defaults to 10 minutes,
/// generous enough for huge monomorph fan-outs but tight enough
/// that an unbounded `opt -O3` blowup turns into a build failure
/// instead of a process holding the runner forever.
/// Target CPU passed to `opt` and `llc`. Defaults to `native`
/// (matching `rustc -C target-cpu=native`); `GOS_LLVM_MCPU` lets
/// callers override — `x86-64-v3` is the documented escape hatch
/// for short-running benchmarks where the AVX-512 dirty-state
/// transition penalty dominates the savings (§5 release-perf
/// investigation, fannkuch).
fn mcpu_target() -> String {
    std::env::var("GOS_LLVM_MCPU").unwrap_or_else(|_| "native".to_string())
}

fn opt_timeout() -> std::time::Duration {
    let secs = std::env::var("GOS_LLVM_OPT_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(600);
    std::time::Duration::from_secs(secs)
}

/// Spawns `cmd`, waits up to `timeout`, and surfaces a clear error
/// when the subprocess exceeds the cap (kills the child first so
/// it doesn't outlive the build). Captures stdout / stderr through
/// a 64 KiB-per-stream cap so a runaway `opt`/`llc` diagnostic
/// stream cannot grow unbounded. The polling cadence (50 ms) keeps
/// the steady-state overhead negligible compared to `opt -O3`'s
/// usual runtime.
fn run_with_timeout(
    mut cmd: std::process::Command,
    timeout: std::time::Duration,
    tool: &str,
) -> Result<std::process::Output> {
    use std::io::Read;

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("spawn {tool}"))?;
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_thread = stdout_pipe.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            cap_diagnostic_stream(buf)
        })
    });
    let stderr_thread = stderr_pipe.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            cap_diagnostic_stream(buf)
        })
    });
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(anyhow!(
                        "{tool} exceeded {secs}s timeout (set GOS_LLVM_OPT_TIMEOUT_SECS to raise it)",
                        secs = timeout.as_secs(),
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow!("{tool} wait failed: {e}"));
            }
        }
    };
    let stdout = stdout_thread
        .map(|t| t.join().unwrap_or_default())
        .unwrap_or_default();
    let stderr = stderr_thread
        .map(|t| t.join().unwrap_or_default())
        .unwrap_or_default();
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// Caps a captured subprocess stream at the last 64 KiB. LLVM tools
/// occasionally emit hundreds of MB of repetitive diagnostics (e.g.
/// instcombine loops on pathological IR); without a cap, the parent
/// build process would mirror that growth in RSS while waiting for
/// the child to exit. The tail is kept rather than the head because
/// the actionable error (the failure point) is invariably last.
fn cap_diagnostic_stream(buf: Vec<u8>) -> Vec<u8> {
    const CAP: usize = 64 * 1024;
    if buf.len() <= CAP {
        return buf;
    }
    let trimmed_start = buf.len() - CAP;
    let mut out = Vec::with_capacity(CAP + 64);
    out.extend_from_slice(
        format!("[diagnostic stream truncated: dropped {trimmed_start} bytes]\n").as_bytes(),
    );
    out.extend_from_slice(&buf[trimmed_start..]);
    out
}

fn find_opt() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("GOS_LLVM_OPT") {
        return Ok(PathBuf::from(path));
    }
    for candidate in [
        "opt",
        "opt-18",
        "opt-19",
        "opt-20",
        "opt-17",
        "/home/daniel/dev/.local-llvm-18/usr/lib/llvm-18/bin/opt",
        "/usr/lib/llvm-18/bin/opt",
        "/usr/lib/llvm-19/bin/opt",
        "/usr/lib/llvm-20/bin/opt",
    ] {
        if is_executable(candidate) {
            return Ok(PathBuf::from(candidate));
        }
    }
    Err(anyhow!(
        "opt (LLVM optimiser) not found. Install `llvm-18-dev` or set \
         GOS_LLVM_OPT to the full path."
    ))
}

fn find_llc() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("GOS_LLC") {
        return Ok(PathBuf::from(path));
    }
    // Well-known system paths and versioned binaries for
    // apt-installed LLVM on Debian/Ubuntu.
    for candidate in [
        "llc",
        "llc-18",
        "llc-19",
        "llc-20",
        "llc-17",
        "/home/daniel/dev/.local-llvm-18/usr/lib/llvm-18/bin/llc",
        "/usr/lib/llvm-18/bin/llc",
        "/usr/lib/llvm-19/bin/llc",
        "/usr/lib/llvm-20/bin/llc",
    ] {
        if is_executable(candidate) {
            return Ok(PathBuf::from(candidate));
        }
    }
    Err(anyhow!(
        "llc not found. Install `llvm-18-dev` or similar, or set GOS_LLC \
         to the full path."
    ))
}

fn is_executable(path: &str) -> bool {
    if let Ok(meta) = std::fs::metadata(path) {
        return meta.is_file();
    }
    // Fall back to a `which`-style PATH scan for bare names.
    if !path.contains('/') {
        if let Ok(paths) = std::env::var("PATH") {
            for dir in paths.split(':') {
                let p = format!("{dir}/{path}");
                if std::fs::metadata(&p).is_ok_and(|m| m.is_file()) {
                    return true;
                }
            }
        }
    }
    false
}

fn host_triple() -> String {
    // Mirror the target triple the Cranelift backend uses via
    // `cranelift_native`. Linux hosts are effectively always
    // `x86_64-unknown-linux-gnu` or `aarch64-unknown-linux-gnu`
    // these days; honour `TARGET` (the env var cargo sets for
    // build scripts) when present.
    if let Ok(triple) = std::env::var("TARGET") {
        return triple;
    }
    // Fall back to `uname -m` + linux-gnu.
    let arch = std::process::Command::new("uname")
        .arg("-m")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "x86_64".to_string(), |s| s.trim().to_string());
    format!("{arch}-unknown-linux-gnu")
}

#[cfg(test)]
mod shape_validation_tests {
    use super::validate_global_decl_shape;

    #[test]
    fn accepts_constant_definition() {
        let g = "@.str_0 = private unnamed_addr constant [6 x i8] c\"hello\\00\"";
        assert!(validate_global_decl_shape(g).is_ok());
    }

    #[test]
    fn accepts_extern_global() {
        let g = "@GOS_RT_STDOUT_LEN = external local_unnamed_addr global i64";
        assert!(validate_global_decl_shape(g).is_ok());
    }

    #[test]
    fn accepts_function_declaration() {
        let g = "declare void @gos_rt_print_str(ptr)";
        assert!(validate_global_decl_shape(g).is_ok());
    }

    #[test]
    fn rejects_bare_identifier() {
        // The exact regression shape: a runtime symbol name
        // accidentally inserted as a bare string instead of a
        // full `@name = constant ...` declaration.
        let g = "gos_rt_arena_save";
        let err = validate_global_decl_shape(g).unwrap_err();
        assert!(
            err.to_string().contains("malformed module-level entry"),
            "expected shape diagnostic, got: {err}"
        );
    }

    #[test]
    fn rejects_random_text() {
        let g = "this is not LLVM IR";
        assert!(validate_global_decl_shape(g).is_err());
    }
}
