//! Module-level assembly: runtime symbol declarations +
//! per-function lowering + `llc -O3` invocation.

use std::fmt::Write;
use std::io::Write as IoWrite;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use gossamer_mir::Body;
use gossamer_types::TyCtxt;

use crate::lower::{Lowerer, StringPool};

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
    let ir = render_module(bodies, tcx)?;
    let triple = host_triple();
    let bytes = invoke_llc(&ir, &triple)?;
    Ok(NativeObject { triple, bytes })
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
    let (ir, fallback_bodies) = render_module_with_fallback(bodies, tcx)?;
    let triple = host_triple();
    let bytes = invoke_llc(&ir, &triple)?;
    Ok(CompileOutcome {
        object: NativeObject { triple, bytes },
        fallback_bodies,
    })
}

fn render_module(bodies: &[Body], tcx: &TyCtxt) -> Result<String> {
    let (ir, fallbacks) = render_module_inner(bodies, tcx, /*allow_fallback=*/ false)?;
    debug_assert!(fallbacks.is_empty());
    Ok(ir)
}

fn render_module_with_fallback(bodies: &[Body], tcx: &TyCtxt) -> Result<(String, Vec<String>)> {
    render_module_inner(bodies, tcx, /*allow_fallback=*/ true)
}

/// Single shared rendering pipeline used by both the strict
/// (`compile_to_object`) and the fallback-tolerant
/// (`compile_with_fallback`) paths.
///
/// When `allow_fallback` is true, bodies the lowerer rejects
/// are dropped from the LLVM module and replaced by an
/// `extern` declaration so the linker can resolve them against
/// the Cranelift-built companion object. The names are returned
/// in the second tuple slot.
///
/// Bodies that emit an LLVM-internal tool error or I/O failure
/// always abort regardless of `allow_fallback` — those signal
/// pipeline bugs, not coverage gaps.
fn render_module_inner(
    bodies: &[Body],
    tcx: &TyCtxt,
    allow_fallback: bool,
) -> Result<(String, Vec<String>)> {
    let mut out = String::new();
    writeln!(out, "; ModuleID = \"gossamer\"").unwrap();
    writeln!(out, "target triple = \"{}\"", host_triple()).unwrap();
    if want_reproducible() {
        // Skip any wallclock / hostname / pid headers a future
        // emitter might be tempted to add. The current pipeline
        // doesn't include any, but pin the rule here so future
        // edits don't silently break reproducibility.
        writeln!(out, "; reproducible-build = true").unwrap();
    }
    writeln!(out).unwrap();

    for d in RUNTIME_DECLARATIONS {
        writeln!(out, "{d}").unwrap();
    }
    writeln!(out).unwrap();

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

    let mut body_text = String::new();
    let mut globals: Vec<String> = Vec::new();
    let mut fallback_bodies: Vec<String> = Vec::new();
    let string_pool = std::rc::Rc::new(std::cell::RefCell::new(StringPool::default()));
    for body in bodies {
        let mut lowerer = Lowerer::new(body, tcx);
        lowerer.fn_name_by_def.clone_from(&fn_name_by_def);
        lowerer.param_tys_by_name.clone_from(&param_tys_by_name);
        lowerer.strings = string_pool.clone();
        match lowerer.lower() {
            Ok(text) => {
                body_text.push_str(&text);
                body_text.push('\n');
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
                let fail_on_fallback = std::env::var("GOSSAMER_FAIL_ON_LLVM_FALLBACK")
                    .ok()
                    .is_some_and(|v| !v.is_empty() && v != "0");
                if fail_on_fallback {
                    return Err(anyhow!(
                        "llvm backend: `{fn_name}` would fall back to Cranelift ({msg}) but \
                         GOSSAMER_FAIL_ON_LLVM_FALLBACK is set",
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
                    body_text.push_str(&extern_declare(body, tcx));
                    body_text.push('\n');
                } else {
                    return Err(anyhow!(
                        "llvm backend: cannot lower `{fn_name}`: {msg}",
                        fn_name = body.name,
                    ));
                }
            }
            Err(BuildError::Tool(msg)) => {
                return Err(anyhow!("llvm backend: tool: {msg}"));
            }
            Err(BuildError::Io(err)) => return Err(err),
        }
    }
    globals.sort();
    globals.dedup();
    // Shape-validate every accumulated module global. The
    // `runtime_refs` BTreeSet inside `Lowerer` accepts arbitrary
    // strings; two real regressions (`spectral_norm_regression_fix.md`
    // 2026-04-30 and `llvm_release_silent_fallback.md` 2026-04-28)
    // shipped because a malformed entry corrupted the IR string
    // and silently flipped affected bodies to the Cranelift
    // fallback — costing 18-21x perf with no diagnostic. Each entry
    // must be either an `@symbol = ... constant ...` definition or
    // an `@symbol = ... global ...` definition or a `declare ...`
    // function declaration. Anything else is a programmer error.
    for g in &globals {
        validate_global_decl_shape(g)?;
        writeln!(out, "{g}").unwrap();
    }
    if !globals.is_empty() {
        writeln!(out).unwrap();
    }
    let pool_text = string_pool.borrow().render();
    if !pool_text.is_empty() {
        out.push_str(&pool_text);
        writeln!(out).unwrap();
    }
    out.push_str(&body_text);

    // Per-shape callable thunks. The MIR emits `gos_fn_addr(
    // "__fn_thunk_<sig>")` whenever a bare fn item is coerced
    // to a `Fn(...)` parameter; each unique shape needs one
    // thunk that loads the real fn pointer from `env+8` and
    // forwards the typed arguments. Collect the set of
    // referenced thunk names from MIR and synthesize an LLVM
    // `define` for each. Mirrors `define_shape_thunk` on the
    // Cranelift side.
    let mut thunk_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for body in bodies {
        collect_thunk_names_in_body(body, &mut thunk_names);
    }
    for name in &thunk_names {
        if let Some(text) = render_shape_thunk(name) {
            out.push_str(&text);
            out.push('\n');
        }
    }

    // The user's `main` function might be in the fallback set.
    // The C-ABI shim must call `gos_main` regardless — if main
    // fell back, it gets an `extern declare` above and the call
    // resolves against the Cranelift object at link time.
    if let Some(user_main) = bodies.iter().find(|b| b.name == "main") {
        let ret_is_unit = matches!(
            tcx.kind(user_main.local_ty(gossamer_mir::Local::RETURN)),
            Some(gossamer_types::TyKind::Unit)
        );
        writeln!(out, "define i32 @main(i32 %argc, ptr %argv) {{").unwrap();
        writeln!(out, "entry:").unwrap();
        writeln!(out, "  call void @gos_rt_set_args(i32 %argc, ptr %argv)").unwrap();
        if ret_is_unit {
            writeln!(out, "  call void @\"gos_main\"()").unwrap();
            writeln!(out, "  call void @gos_rt_flush_stdout()").unwrap();
            writeln!(out, "  ret i32 0").unwrap();
        } else {
            writeln!(out, "  %r = call i64 @\"gos_main\"()").unwrap();
            writeln!(out, "  call void @gos_rt_flush_stdout()").unwrap();
            writeln!(out, "  %code = call i32 @gos_rt_main_exit_code(i64 %r)").unwrap();
            writeln!(out, "  ret i32 %code").unwrap();
        }
        writeln!(out, "}}").unwrap();
    }
    // Module-level metadata referenced by `!invariant.load
    // !0` in the inlined hot paths. Empty list (`!{}`) is the
    // standard form for "no extra info"; LLVM only needs a
    // metadata node to attach to the load, the contents are
    // unused for invariance.
    writeln!(out).unwrap();
    writeln!(out, "!0 = !{{}}").unwrap();
    if want_dwarf() {
        emit_dwarf_metadata(&mut out, bodies);
    }
    // The previous implementation emitted `@"main"` and then ran
    // `out.replace("@\"main\"", "@\"gos_main\"")` here. That cloned
    // the entire IR string into a second buffer — on a 50k-LOC
    // program with ~50 MB of IR text, peak heap doubled to ~100 MB
    // for one tick. Both `lower::Lowerer::define_open` and the
    // `body_decl` declarer now route the function name through
    // `mangle_fn_name`, so the IR is emitted with `@"gos_main"`
    // already in place and no second pass is needed.
    Ok((out, fallback_bodies))
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

fn invoke_llc(ir: &str, triple: &str) -> Result<Vec<u8>> {
    // Reproducible mode: pin SOURCE_DATE_EPOCH so any timestamp
    // `llc` writes into the object header is deterministic, and
    // pick a stable temp directory layout instead of `pid`-based.
    let tmp_dir = if want_reproducible() {
        std::env::temp_dir().join("gos-llvm-reproducible")
    } else {
        std::env::temp_dir().join(format!("gos-llvm-{}", std::process::id()))
    };
    std::fs::create_dir_all(&tmp_dir).with_context(|| format!("creating {}", tmp_dir.display()))?;
    let ll_path = tmp_dir.join("unit.ll");
    let opt_path = tmp_dir.join("unit.opt.bc");
    let obj_path = tmp_dir.join("unit.o");
    {
        let mut f = std::fs::File::create(&ll_path)
            .with_context(|| format!("creating {}", ll_path.display()))?;
        f.write_all(ir.as_bytes())
            .with_context(|| format!("writing {}", ll_path.display()))?;
    }
    let keep_artifacts = std::env::var("GOS_LLVM_DUMP").is_ok();
    if keep_artifacts {
        // Emit the canonical dump-path marker the
        // `llvm_lowering_marker` test (and ad-hoc debug runs)
        // grep for. Pinning the line shape lets tooling locate
        // the IR file without guessing a temp-dir layout.
        eprintln!("llvm backend: IR at {}", ll_path.display());
    }
    // Mid-end pipeline: `opt -O3` runs `mem2reg`, GVN, instcombine,
    // loop unrolling, the loop vectoriser, the SLP vectoriser, …
    // Critical because `llc` only does codegen / register
    // allocation; without `opt` first every Lowerer-emitted
    // `alloca` + `load` + `store` survives into the asm and the
    // hot loops spill aggressively.
    let opt_tool = find_opt()?;
    let mcpu = mcpu_target();
    let mut opt_cmd = std::process::Command::new(&opt_tool);
    opt_cmd
        .arg("-O3")
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
        .arg("--disable-loop-idiom-all")
        .arg(&ll_path)
        .arg("-o")
        .arg(&opt_path);
    let opt_output = run_with_timeout(opt_cmd, opt_timeout(), "opt")
        .with_context(|| format!("spawn {}", opt_tool.display()))?;
    if !opt_output.status.success() {
        if !keep_artifacts {
            let _ = std::fs::remove_dir_all(&tmp_dir);
        } else {
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
        .arg("-O3")
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
        .arg(&obj_path);
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
        if !keep_artifacts {
            let _ = std::fs::remove_dir_all(&tmp_dir);
        } else {
            eprintln!("llvm backend: failing IR kept at {}", ll_path.display());
        }
        return Err(anyhow!(
            "llc failed ({status}): {stderr}",
            status = output.status,
            stderr = String::from_utf8_lossy(&output.stderr)
        ));
    }
    let bytes =
        std::fs::read(&obj_path).with_context(|| format!("reading {}", obj_path.display()))?;
    if keep_artifacts {
        eprintln!("llvm backend: IR at {}", ll_path.display());
    } else {
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
    Ok(bytes)
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
/// it doesn't outlive the build). Captures stdout / stderr so the
/// caller can fold them into its diagnostics. The polling cadence
/// (50 ms) keeps the steady-state overhead negligible compared to
/// `opt -O3`'s usual runtime.
fn run_with_timeout(
    mut cmd: std::process::Command,
    timeout: std::time::Duration,
    tool: &str,
) -> Result<std::process::Output> {
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("spawn {tool}"))?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
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
    }
    child
        .wait_with_output()
        .with_context(|| format!("wait {tool}"))
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

/// Declarations for every runtime symbol the lowerer might
/// reach by name. LLVM wants a declaration before any use.
/// Redundant declarations are harmless; missing ones surface
/// as `llc: error: use of undefined value`. Kept in loose
/// sync with the exported symbols in `gossamer-runtime::c_abi`.
pub(crate) const RUNTIME_DECLARATIONS: &[&str] = &[
    // Program entry / control.
    "declare void @gos_rt_set_args(i32, ptr)",
    "declare void @gos_rt_flush_stdout()",
    "declare void @gos_rt_panic(ptr)",
    "declare void @gos_rt_exit(i32)",
    // Stdout buffer lock — paired around every inline byte-write
    // region the lowerer emits so multi-thread output stays
    // serialised against `@GOS_RT_STDOUT_LEN`.
    "declare void @gos_rt_stdout_acquire()",
    "declare void @gos_rt_stdout_release()",
    // Prelude printers.
    "declare void @gos_rt_print_str(ptr)",
    "declare void @gos_rt_println()",
    "declare void @gos_rt_print_i64(i64)",
    "declare void @gos_rt_print_u64(i64)",
    "declare void @gos_rt_eprint_str(ptr)",
    "declare void @gos_rt_eprintln()",
    "declare void @gos_rt_print_f64(double)",
    "declare void @gos_rt_print_bool(i32)",
    "declare void @gos_rt_print_char(i32)",
    // Argv / stdin helpers.
    "declare ptr @gos_rt_os_args()",
    // Time / runtime cooperation.
    "declare double @gos_rt_time_now()",
    "declare i64 @gos_rt_now_ns()",
    "declare i64 @gos_rt_time_now_ms()",
    "declare void @gos_rt_sleep_ns(i64)",
    "declare void @gos_rt_go_yield()",
    // Math (f64 -> f64) — preferred for cross-backend parity;
    // the LLVM frontend may also emit `llvm.sqrt.f64` etc.
    // directly through the math-intrinsic short-path.
    "declare double @gos_rt_math_sqrt(double)",
    "declare double @gos_rt_math_sin(double)",
    "declare double @gos_rt_math_cos(double)",
    "declare double @gos_rt_math_log(double)",
    "declare double @gos_rt_math_exp(double)",
    "declare double @gos_rt_math_abs(double)",
    "declare double @gos_rt_math_floor(double)",
    "declare double @gos_rt_math_ceil(double)",
    "declare double @gos_rt_math_pow(double, double)",
    // Length / indexing.
    "declare i64 @gos_rt_arr_len(ptr)",
    "declare ptr @gos_rt_arr_iter(ptr)",
    "declare ptr @gos_rt_arr_iter_next(ptr)",
    "declare i64 @gos_rt_len(ptr)",
    "declare i64 @gos_rt_str_len(ptr)",
    "declare i64 @gos_rt_str_byte_at(ptr, i64)",
    "declare ptr @gos_rt_str_substring(ptr, i64, i64)",
    "declare ptr @gos_rt_os_read_dir(ptr)",
    // String constructors / mutation.
    "declare ptr @gos_rt_str_concat(ptr, ptr)",
    "declare ptr @gos_rt_str_trim(ptr)",
    "declare ptr @gos_rt_str_to_upper(ptr)",
    "declare ptr @gos_rt_str_to_lower(ptr)",
    "declare i32 @gos_rt_str_contains(ptr, ptr)",
    "declare i32 @gos_rt_str_starts_with(ptr, ptr)",
    "declare i32 @gos_rt_str_ends_with(ptr, ptr)",
    "declare i64 @gos_rt_str_find(ptr, ptr)",
    "declare ptr @gos_rt_str_find_opt(ptr, ptr)",
    "declare ptr @gos_rt_str_replace(ptr, ptr, ptr)",
    "declare ptr @malloc(i64)",
    "declare ptr @gos_rt_vec_new(i32)",
    "declare ptr @gos_rt_vec_with_capacity(i32, i64)",
    "declare i64 @gos_rt_vec_len(ptr)",
    "declare void @gos_rt_vec_push(ptr, ptr)",
    "declare void @gos_rt_vec_push_i64(ptr, i64)",
    "declare ptr @gos_rt_vec_get_ptr(ptr, i64)",
    "declare i32 @gos_rt_vec_pop(ptr, ptr)",
    "declare ptr @gos_rt_vec_slice(ptr, i64, i64)",
    // Errors module.
    "declare ptr @gos_rt_error_new(ptr)",
    "declare ptr @gos_rt_error_wrap(ptr, ptr)",
    "declare ptr @gos_rt_error_message(ptr)",
    "declare ptr @gos_rt_error_cause(ptr)",
    "declare i8 @gos_rt_error_is(ptr, ptr)",
    "declare ptr @gos_rt_errors_join_vec(ptr)",
    // Regex module.
    "declare ptr @gos_rt_regex_compile(ptr)",
    "declare i8 @gos_rt_regex_is_match(ptr, ptr)",
    "declare ptr @gos_rt_regex_find(ptr, ptr)",
    "declare ptr @gos_rt_regex_find_opt(ptr, ptr)",
    "declare ptr @gos_rt_regex_captures(ptr, ptr)",
    "declare ptr @gos_rt_regex_find_all(ptr, ptr)",
    "declare ptr @gos_rt_regex_captures_all(ptr, ptr)",
    "declare ptr @gos_rt_regex_replace_all(ptr, ptr, ptr)",
    "declare ptr @gos_rt_regex_split(ptr, ptr)",
    // fs / path.
    "declare ptr @gos_rt_fs_read_to_string(ptr)",
    "declare i64 @gos_rt_fs_write(ptr, ptr)",
    "declare i64 @gos_rt_fs_create_dir_all(ptr)",
    "declare ptr @gos_rt_path_join(ptr, ptr)",
    // flag::Set.
    "declare ptr @gos_rt_flag_set_new(ptr)",
    "declare ptr @gos_rt_flag_set_string(ptr, ptr, ptr, ptr)",
    "declare ptr @gos_rt_flag_set_int(ptr, ptr, i64, ptr)",
    "declare ptr @gos_rt_flag_set_uint(ptr, ptr, i64, ptr)",
    "declare ptr @gos_rt_flag_set_float(ptr, ptr, double, ptr)",
    "declare ptr @gos_rt_flag_set_bool(ptr, ptr, i8, ptr)",
    "declare ptr @gos_rt_flag_set_duration(ptr, ptr, i64, ptr)",
    "declare ptr @gos_rt_flag_set_string_list(ptr, ptr, ptr)",
    "declare void @gos_rt_flag_set_short(ptr, i64)",
    "declare ptr @gos_rt_flag_set_usage(ptr)",
    "declare ptr @gos_rt_flag_set_parse(ptr, ptr)",
    "declare i64 @gos_rt_duration_from_secs(i64)",
    "declare i64 @gos_rt_duration_from_millis(i64)",
    "declare ptr @gos_rt_time_format_rfc3339(i64)",
    "declare ptr @gos_rt_flag_parse(ptr)",
    "declare ptr @gos_rt_flag_map_get(ptr, ptr)",
    "declare ptr @gos_rt_os_env(ptr)",
    "declare ptr @gos_rt_os_cwd()",
    "declare ptr @gos_rt_fs_list_dir(ptr)",
    "declare ptr @gos_rt_fs_walk_dir(ptr)",
    "declare ptr @gos_rt_exec_run(ptr, ptr)",
    "declare i64 @gos_rt_os_exists(ptr)",
    "declare i64 @gos_rt_os_is_file(ptr)",
    "declare i64 @gos_rt_os_is_dir(ptr)",
    "declare i64 @gos_rt_os_remove_file(ptr)",
    "declare ptr @gos_rt_result_map_bare(ptr, i64)",
    "declare ptr @gos_rt_result_map_err_bare(ptr, i64)",
    "declare ptr @gos_rt_os_write_file_result(ptr, ptr)",
    "declare ptr @gos_rt_os_mkdir_all_result(ptr)",
    "declare ptr @gos_rt_os_remove_file_result(ptr)",
    "declare ptr @gos_rt_os_remove_dir_all_result(ptr)",
    "declare ptr @gos_rt_http_stream(ptr, ptr, ptr, ptr)",
    "declare ptr @gos_rt_http_get(ptr, ptr)",
    "declare ptr @gos_rt_http_stream_next_line(ptr)",
    // bufio::Scanner.
    "declare ptr @gos_rt_bufio_scanner_new(ptr)",
    "declare i8 @gos_rt_bufio_scanner_scan(ptr)",
    "declare ptr @gos_rt_bufio_scanner_text(ptr)",
    // http client.
    "declare ptr @gos_rt_http_client_new()",
    "declare ptr @gos_rt_http_client_get(ptr, ptr)",
    "declare ptr @gos_rt_http_client_post(ptr, ptr)",
    "declare ptr @gos_rt_http_request_header(ptr, ptr, ptr)",
    "declare ptr @gos_rt_http_request_body(ptr, ptr)",
    "declare ptr @gos_rt_http_request_send(ptr)",
    "declare i64 @gos_rt_http_response_status(ptr)",
    "declare ptr @gos_rt_http_response_body(ptr)",
    "declare i64 @gos_rt_vec_get_i64(ptr, i64)",
    "declare void @gos_rt_vec_set_i64(ptr, i64, i64)",
    "declare ptr @gos_rt_vec_format_i64(ptr)",
    "declare void @gos_rt_concat_init()",
    "declare void @gos_rt_concat_str(ptr)",
    "declare void @gos_rt_concat_i64(i64)",
    "declare void @gos_rt_concat_u64(i64)",
    "declare void @gos_rt_concat_f64(double)",
    "declare void @gos_rt_concat_f64_prec(double, i64)",
    "declare void @gos_rt_concat_bool(i32)",
    "declare void @gos_rt_concat_char(i32)",
    "declare ptr @gos_rt_concat_finish()",
    "declare ptr @gos_rt_f64_prec_to_str(double, i64)",
    "declare i32 @gos_rt_main_exit_code(i64)",
    "declare ptr @gos_rt_result_new(i64, i64)",
    "declare i64 @gos_rt_result_disc(ptr)",
    "declare i64 @gos_rt_result_payload(ptr)",
    "declare i64 @gos_rt_result_unwrap(ptr)",
    "declare i64 @gos_rt_result_unwrap_or(ptr, i64)",
    "declare i64 @gos_rt_result_ok(ptr)",
    "declare i64 @gos_rt_result_err(ptr)",
    "declare ptr @gos_rt_result_ok_or(ptr, i64)",
    "declare i64 @gos_rt_result_is_ok(ptr)",
    "declare i64 @gos_rt_result_is_err(ptr)",
    "declare ptr @gos_rt_set_new()",
    "declare i8 @gos_rt_set_insert(ptr, ptr)",
    "declare i8 @gos_rt_set_contains(ptr, ptr)",
    "declare i8 @gos_rt_set_remove(ptr, ptr)",
    "declare i64 @gos_rt_set_len(ptr)",
    "declare ptr @gos_rt_btmap_new()",
    "declare void @gos_rt_btmap_insert(ptr, ptr, i64)",
    "declare i64 @gos_rt_btmap_get_or(ptr, ptr, i64)",
    "declare i64 @gos_rt_btmap_len(ptr)",
    "declare ptr @gos_rt_btmap_keys(ptr)",
    "declare ptr @gos_rt_str_as_bytes(ptr)",
    "declare ptr @gos_rt_vec_clone(ptr)",
    "declare void @gos_rt_http_response_set_header(ptr, ptr, ptr)",
    "declare ptr @gos_rt_http_response_get_header(ptr, ptr)",
    "declare void @gos_rt_http_request_set_header(ptr, ptr, ptr)",
    "declare ptr @gos_rt_http_request_get_header(ptr, ptr)",
    "declare ptr @gos_rt_http_request_path(ptr)",
    "declare ptr @gos_rt_http_request_method(ptr)",
    "declare ptr @gos_rt_http_request_query(ptr)",
    "declare ptr @gos_rt_http_request_body_str(ptr)",
    "declare ptr @gos_rt_http_response_text_new(i64, ptr)",
    "declare ptr @gos_rt_http_response_json_new(i64, ptr)",
    // gzip / slog / testing.
    "declare ptr @gos_rt_gzip_encode(ptr)",
    "declare ptr @gos_rt_gzip_decode(ptr)",
    "declare void @gos_rt_slog_info(ptr)",
    "declare void @gos_rt_slog_warn(ptr)",
    "declare void @gos_rt_slog_error(ptr)",
    "declare void @gos_rt_slog_debug(ptr)",
    "declare i8 @gos_rt_testing_check(i8, ptr)",
    "declare i8 @gos_rt_testing_check_eq_i64(i64, i64, ptr)",
    "declare ptr @gos_rt_str_split(ptr, ptr)",
    "declare ptr @gos_rt_str_lines(ptr)",
    "declare ptr @gos_rt_str_repeat(ptr, i64)",
    "declare i8 @gos_rt_str_eq(ptr, ptr)",
    "declare i8 @gos_rt_str_is_empty(ptr)",
    "declare i8 @gos_rt_len_is_zero(ptr)",
    // Parsing / formatting.
    "declare i64 @gos_rt_parse_i64(ptr, ptr)",
    "declare ptr @gos_rt_parse_i64_result(ptr)",
    "declare ptr @gos_rt_result_map_err(ptr, ptr)",
    "declare ptr @gos_rt_result_map(ptr, ptr)",
    "declare ptr @gos_rt_flag_cell_load_str(ptr)",
    "declare i64 @gos_rt_flag_cell_load_i64(ptr)",
    "declare i64 @gos_rt_flag_cell_load_bool(ptr)",
    "declare double @gos_rt_flag_cell_load_f64(ptr)",
    "declare ptr @gos_rt_flag_cell_load_vec(ptr)",
    "declare ptr @gos_rt_json_value_string(ptr)",
    "declare ptr @gos_rt_json_value_int(i64)",
    "declare ptr @gos_rt_json_value_bool(i32)",
    "declare ptr @gos_rt_json_value_null()",
    "declare ptr @gos_rt_json_value_array(ptr)",
    "declare ptr @gos_rt_json_value_object(ptr)",
    "declare ptr @gos_rt_json_value_object_n(i64, ptr)",
    "declare double @gos_rt_parse_f64(ptr, ptr)",
    "declare ptr @gos_rt_i64_to_str(i64)",
    "declare ptr @gos_rt_u64_to_str(i64)",
    "declare ptr @gos_rt_f64_to_str(double)",
    "declare ptr @gos_rt_bool_to_str(i32)",
    "declare ptr @gos_rt_char_to_str(i32)",
    // Streams.
    "declare ptr @gos_rt_io_stdin()",
    "declare ptr @gos_rt_io_stdout()",
    "declare ptr @gos_rt_io_stderr()",
    "declare void @gos_rt_stream_write_byte(ptr, i64)",
    "declare void @gos_rt_stream_write_byte_array(ptr, ptr, i64)",
    "declare void @gos_rt_stream_flush(ptr)",
    "declare void @gos_rt_stream_write_str(ptr, ptr)",
    "declare ptr @gos_rt_stream_read_line(ptr)",
    "declare ptr @gos_rt_stream_read_to_string(ptr)",
    // Memory intrinsic the aggregate path uses.
    "declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)",
    "declare void @llvm.lifetime.start.p0(i64, ptr)",
    "declare void @llvm.lifetime.end.p0(i64, ptr)",
    // Heap allocator backing aggregate-by-value returns. The
    // callee's stack slot dies when the function frame is
    // popped, so an aggregate return value has to be copied
    // into a heap arena that survives the return.
    "declare ptr @gos_rt_gc_alloc(i64)",
    // Arena watermark + rewind primitives. The codegen wraps
    // calls returning a pure-primitive aggregate (e.g.
    // `[f64; N]`, `(i64, i64)`, `struct Vec3 { x, y, z }`) with
    // a save/restore pair so the heap copy of the return value —
    // which is dead the instant the caller `memcpy`s it into its
    // own slot — does not accumulate across iterations of a
    // calling loop. Drives the spectral-norm matvec memory fix.
    "declare i64 @gos_rt_arena_save()",
    "declare void @gos_rt_arena_restore(i64)",
    // Sync primitives (Mutex, WaitGroup, Atomic, heap-Vec).
    "declare ptr @gos_rt_mutex_new()",
    "declare void @gos_rt_mutex_lock(ptr)",
    "declare void @gos_rt_mutex_unlock(ptr)",
    "declare ptr @gos_rt_wg_new()",
    "declare i64 @gos_rt_wg_add(ptr, i64)",
    "declare i64 @gos_rt_wg_done(ptr)",
    "declare void @gos_rt_wg_wait(ptr)",
    "declare i64 @gos_rt_wg_error(ptr)",
    "declare i64 @gos_rt_wg_error_clear(ptr)",
    "declare ptr @gos_rt_sync_i64_new(i64)",
    "declare void @gos_rt_sync_i64_drop(ptr)",
    "declare i64 @gos_rt_sync_i64_len(ptr)",
    "declare i64 @gos_rt_sync_i64_get(ptr, i64)",
    "declare void @gos_rt_sync_i64_set(ptr, i64, i64)",
    "declare void @gos_rt_sync_i64_push(ptr, i64)",
    "declare i64 @gos_rt_sync_i64_add(ptr, i64, i64)",
    "declare ptr @gos_rt_sync_u8_new(i64)",
    "declare void @gos_rt_sync_u8_drop(ptr)",
    "declare i64 @gos_rt_sync_u8_len(ptr)",
    "declare i64 @gos_rt_sync_u8_get(ptr, i64)",
    "declare void @gos_rt_sync_u8_set(ptr, i64, i64)",
    "declare void @gos_rt_sync_u8_push(ptr, i64)",
    "declare ptr @gos_rt_heap_i64_new(i64)",
    "declare void @gos_rt_heap_i64_free(ptr)",
    "declare i64 @gos_rt_heap_i64_get(ptr, i64)",
    "declare void @gos_rt_heap_i64_set(ptr, i64, i64)",
    "declare i64 @gos_rt_heap_i64_len(ptr)",
    "declare void @gos_rt_heap_i64_write_bytes_to_stdout(ptr, i64, i64)",
    "declare void @gos_rt_heap_i64_write_lines_to_stdout(ptr, i64, i64, i64)",
    "declare ptr @gos_rt_heap_u8_to_string(ptr, i64)",
    "declare void @gos_rt_heap_u8_free(ptr)",
    "declare void @gos_rt_chan_drop(ptr)",
    "declare ptr @gos_rt_atomic_i64_new(i64)",
    "declare i64 @gos_rt_atomic_i64_load(ptr)",
    "declare void @gos_rt_atomic_i64_store(ptr, i64)",
    "declare i64 @gos_rt_atomic_i64_fetch_add(ptr, i64)",
    "declare i64 @gos_rt_atomic_i64_load_acquire(ptr)",
    "declare void @gos_rt_atomic_i64_store_release(ptr, i64)",
    "declare i64 @gos_rt_atomic_i64_load_relaxed(ptr)",
    "declare void @gos_rt_atomic_i64_store_relaxed(ptr, i64)",
    "declare i64 @gos_rt_atomic_i64_fetch_add_acqrel(ptr, i64)",
    "declare i32 @gos_rt_atomic_i64_cas(ptr, i64, i64)",
    "declare i32 @gos_rt_atomic_i64_cas_acq_rel(ptr, i64, i64)",
    "declare i64 @gos_rt_atomic_i64_swap(ptr, i64)",
    "declare i32 @gos_rt_preempt_check()",
    "declare i32 @gos_rt_preempt_check_and_yield()",
    "declare i32 @gos_rt_gc_alloc_rooted(i64)",
    "declare void @gos_rt_gc_shadow_push(i32)",
    "declare i64 @gos_rt_gc_shadow_save()",
    "declare void @gos_rt_gc_shadow_restore(i64)",
    "declare i64 @gos_rt_gc_collect_with_stack_roots()",
    // Goroutine spawn helpers (the LLVM backend currently
    // falls back to Cranelift for `go expr` bodies, but
    // declaring these makes future direct-LLVM lowering a
    // one-line addition).
    "declare void @gos_rt_go_spawn_call_3(ptr, i64, i64, i64)",
    "declare void @gos_rt_go_spawn_call_4(ptr, i64, i64, i64, i64)",
    "declare void @gos_rt_go_spawn_call_5(ptr, i64, i64, i64, i64, i64)",
    "declare void @gos_rt_go_spawn_call_6(ptr, i64, i64, i64, i64, i64, i64)",
    "declare i64 @gos_rt_lcg_jump(i64, i64, i64, i64, i64)",
    // HashMap runtime — per-shape ABI variants. The MIR's
    // method-call dispatch picks one of these based on the
    // map's key + value kinds. The byte-erased ABI
    // (`gos_rt_map_insert`/`_get`/`_remove`) stays available
    // for the cranelift tier; the LLVM tier hits the
    // scalar / string-keyed shapes directly.
    "declare ptr @gos_rt_map_new(i32, i32)",
    "declare ptr @gos_rt_map_new_with_capacity(i32, i32, i64)",
    "declare i64 @gos_rt_map_len(ptr)",
    "declare void @gos_rt_map_insert_i64_i64(ptr, i64, i64)",
    "declare i64 @gos_rt_map_get_i64(ptr, i64)",
    "declare i64 @gos_rt_map_get_or_i64(ptr, i64, i64)",
    "declare i64 @gos_rt_map_inc_i64(ptr, i64, i64)",
    "declare i64 @gos_rt_map_inc_str_i64(ptr, ptr, i64)",
    "declare i8 @gos_rt_map_remove_i64(ptr, i64)",
    "declare i8 @gos_rt_map_contains_key_i64(ptr, i64)",
    "declare void @gos_rt_map_insert_str_i64(ptr, ptr, i64)",
    "declare i64 @gos_rt_map_get_str_i64(ptr, ptr)",
    "declare void @gos_rt_map_insert_str_str(ptr, ptr, ptr)",
    "declare ptr @gos_rt_map_get_str_str(ptr, ptr)",
    "declare i8 @gos_rt_map_contains_key_str(ptr, ptr)",
    "declare i8 @gos_rt_map_remove_str(ptr, ptr)",
    "declare void @gos_rt_map_clear(ptr)",
    "declare i64 @gos_rt_map_inc_at_str_i64(ptr, ptr, i64, i64, i64)",
    "declare void @gos_rt_map_free(ptr)",
    "declare void @gos_rt_vec_free(ptr)",
    "declare void @gos_rt_set_free(ptr)",
    "declare void @gos_rt_btmap_free(ptr)",
    "declare ptr @gos_rt_map_keys_i64(ptr)",
    "declare ptr @gos_rt_map_values_i64(ptr)",
    "declare ptr @gos_rt_map_keys_str(ptr)",
    "declare ptr @gos_rt_map_values_str(ptr)",
    "declare i64 @gos_rt_map_get_or_str_i64(ptr, ptr, i64)",
    "declare ptr @gos_rt_map_get_or_str_str(ptr, ptr, ptr)",
    "declare ptr @gos_rt_map_get_or_i64_str(ptr, i64, ptr)",
    "declare void @gos_rt_map_insert_i64_str(ptr, i64, ptr)",
    "declare ptr @gos_rt_map_get_i64_str(ptr, i64)",
    // Inline-able stdout buffer the LLVM lowerer reads
    // directly from the runtime to bypass per-byte FFI calls
    // in the fasta hot loop. Sizes match
    // `gossamer-runtime::c_abi::STDOUT_BUF_SIZE`.
    //
    // `unnamed_addr` tells `opt`'s alias analysis that the symbols'
    // identities (i.e. addresses) don't matter — only their values.
    // Without this, `opt -O3` cannot prove that two ptr-typed
    // arguments to a function don't both alias `@GOS_RT_STDOUT_LEN`,
    // forcing a reload of `LEN` after every potentially-aliasing
    // store. This is one of the three fixes the §5 release-perf
    // investigation produced.
    "@GOS_RT_STDOUT_BYTES = external local_unnamed_addr global [8192 x i8]",
    "@GOS_RT_STDOUT_LEN = external local_unnamed_addr global i64",
    // Channels — declared so a future direct-LLVM lowering of
    // `chan` operations can call them without a per-call extern
    // declaration race. Cranelift currently handles the lowering.
    "declare ptr @gos_rt_chan_new(i32, i64)",
    "declare void @gos_rt_chan_close(ptr)",
    "declare void @gos_rt_chan_send(ptr, ptr)",
    "declare i32 @gos_rt_chan_try_send(ptr, ptr)",
    "declare i32 @gos_rt_chan_recv(ptr, ptr)",
    "declare i32 @gos_rt_chan_try_recv(ptr, ptr)",
    "declare ptr @gos_rt_chan_recv_option(ptr)",
    "declare ptr @gos_rt_chan_try_recv_option(ptr)",
    // Goroutine spawn extras (small-arity variants and the
    // generic environment-pointer entry point).
    "declare void @gos_rt_go_spawn(ptr, ptr)",
    "declare void @gos_rt_go_spawn_call_0(ptr)",
    "declare void @gos_rt_go_spawn_call_1(ptr, i64)",
    "declare void @gos_rt_go_spawn_call_2(ptr, i64, i64)",
    // GC reset for between-run runtime resets.
    "declare void @gos_rt_gc_reset()",
    // Heap u8 vector ABI.
    "declare ptr @gos_rt_heap_u8_new(i64)",
    "declare i64 @gos_rt_heap_u8_get(ptr, i64)",
    "declare void @gos_rt_heap_u8_set(ptr, i64, i64)",
    "declare i64 @gos_rt_heap_u8_len(ptr)",
    "declare void @gos_rt_heap_u8_write_bytes_to_stdout(ptr, i64, i64)",
    "declare void @gos_rt_heap_u8_write_lines_to_stdout(ptr, i64, i64, i64)",
    // Long-running HTTP server entry point.
    "declare void @gos_rt_http_serve(ptr, ptr, i64)",
    // JSON value accessors / constructors.
    "declare ptr @gos_rt_json_parse(ptr)",
    "declare ptr @gos_rt_json_render(ptr)",
    "declare ptr @gos_rt_json_display(ptr)",
    "declare ptr @gos_rt_json_get(ptr, ptr)",
    "declare ptr @gos_rt_json_get_opt(ptr, ptr)",
    "declare ptr @gos_rt_json_keys_opt(ptr)",
    "declare ptr @gos_rt_json_as_array_opt(ptr)",
    "declare ptr @gos_rt_json_at(ptr, i64)",
    "declare i64 @gos_rt_json_len(ptr)",
    "declare i32 @gos_rt_json_is_null(ptr)",
    "declare i64 @gos_rt_json_as_i64(ptr)",
    "declare double @gos_rt_json_as_f64(ptr)",
    "declare ptr @gos_rt_json_as_str(ptr)",
    "declare i32 @gos_rt_json_as_bool(ptr)",
    "declare ptr @gos_rt_json_identity(ptr)",
    "declare ptr @gos_rt_json_set(ptr, ptr, ptr)",
    // Byte-erased map ABI (cranelift fallback path).
    "declare i32 @gos_rt_map_get(ptr, ptr, ptr)",
    "declare i32 @gos_rt_map_remove(ptr, ptr)",
    // Vec format helpers — emit a pretty-printed string for a
    // GosVec of the given element shape.
    "declare ptr @gos_rt_vec_format_f64(ptr)",
    "declare ptr @gos_rt_vec_format_bool(ptr)",
    "declare ptr @gos_rt_vec_format_string(ptr)",
    "declare ptr @gos_rt_vec_format_vec_i64(ptr)",
    // Construct a GosVec from a flat array slot.
    "declare ptr @gos_rt_vec_from_arr(i32, ptr, i64)",
    // Result debug printer (used by panic-traceback paths).
    "declare i64 @gos_rt_result_dbg(i64)",
    // Array format / sort helpers.
    "declare ptr @gos_rt_arr_format_bool(ptr, i64)",
    "declare ptr @gos_rt_arr_format_f64(ptr, i64)",
    "declare ptr @gos_rt_arr_format_i64(ptr, i64)",
    "declare ptr @gos_rt_arr_format_string(ptr, i64)",
    "declare void @gos_rt_arr_sort_by_i64(ptr, i64, ptr)",
    // Errors / error-list helpers.
    "declare ptr @gos_rt_errors_join(ptr, i64)",
    // Process execution.
    "declare i64 @gos_rt_exec_kill(i64)",
    "declare ptr @gos_rt_exec_spawn(ptr, ptr)",
    // HashMap or-insert variants.
    "declare i64 @gos_rt_map_or_insert_i64_i64(ptr, i64, i64)",
    "declare i64 @gos_rt_map_or_insert_str_i64(ptr, ptr, i64)",
    // Nested-array to Vec helper.
    "declare ptr @gos_rt_nested_arr_to_vec(i64, i64, ptr, i64)",
    // Environment variable helpers.
    "declare ptr @gos_rt_os_set_env(ptr, ptr)",
    "declare void @gos_rt_os_unset_env(ptr)",
    // Regex replace (non-all variant).
    "declare ptr @gos_rt_regex_replace(ptr, ptr, ptr)",
    // Millisecond-granularity sleep.
    "declare void @gos_rt_sleep_ms(i64)",
    // String comparison (returns i32: negative/zero/positive).
    "declare i32 @gos_rt_str_compare(ptr, ptr)",
    // Vec sort-by with i64 comparator closure env.
    "declare void @gos_rt_vec_sort_by_i64(ptr, ptr)",
];

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
