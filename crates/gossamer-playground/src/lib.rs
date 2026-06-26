//! wasm32 backend for the in-browser Gossamer playground.
//!
//! Exposes two `wasm-bindgen` entry points consumed by the web
//! component:
//!
//! - [`run`] compiles `source` through the same parse -> resolve ->
//!   typecheck -> exhaustiveness -> lower pipeline `gos run` uses, then
//!   executes `main` on the bytecode VM with stdout / stderr captured
//!   into buffers.
//! - [`check`] runs only the front-end gate and returns the structured
//!   diagnostics.
//!
//! The Cranelift JIT and the LLVM AOT tiers do not exist on
//! wasm32-unknown-unknown; the interpreter links a no-op JIT stub, so
//! every program runs on the register-based bytecode VM. Host I/O
//! (sockets, filesystem, processes, HTTP server / client, TLS, SQL) and
//! C-library codecs (bzip2, zstd) are unavailable in the browser
//! sandbox and are gated out of the linked standard library; pure
//! computation - strings, collections, math, encoding / JSON, hashing,
//! regex, iterators, formatting - is fully available.

use std::cell::RefCell;

use gossamer_ast::{ItemKind, SourceFile};
use gossamer_diagnostics::{Diagnostic, RenderOptions};
use gossamer_lex::{FileId, SourceMap};
use gossamer_resolve::{ResolveError, resolve_source_file};
use gossamer_types::{ExhaustivenessError, TyCtxt, check_exhaustiveness, typecheck_source_file};
use serde::Serialize;
use wasm_bindgen::prelude::*;

const ENTRY_NAME: &str = "playground.gos";

thread_local! {
    static STDOUT_BUF: RefCell<String> = const { RefCell::new(String::new()) };
    static STDERR_BUF: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Appends VM stdout into the per-thread capture buffer. Installed via
/// `gossamer_interp::set_stdout_writer`, whose `Writer` is a bare
/// `fn(&str)` pointer, so the buffer has to live in a thread-local
/// rather than a captured closure.
fn capture_stdout(text: &str) {
    STDOUT_BUF.with(|b| b.borrow_mut().push_str(text));
}

/// Appends VM stderr into the per-thread capture buffer.
fn capture_stderr(text: &str) {
    STDERR_BUF.with(|b| b.borrow_mut().push_str(text));
}

/// Captured program output plus a terminal error, returned by [`run`].
#[derive(Serialize)]
struct RunResult {
    stdout: String,
    stderr: String,
    error: Option<String>,
    fuel_used: u64,
}

/// One structured diagnostic, returned by [`check`].
#[derive(Serialize)]
struct DiagnosticInfo {
    severity: String,
    message: String,
    line: u32,
    col: u32,
    code: String,
}

/// Front-end diagnostics for a source file, returned by [`check`].
#[derive(Serialize)]
struct CheckResult {
    diagnostics: Vec<DiagnosticInfo>,
}

/// Compiles and executes `source` on the bytecode VM, capturing
/// stdout / stderr. Returns a serialized `RunResult`
/// (`{ stdout, stderr, error, fuel_used }`).
///
/// `fuel` caps loop iterations (default 100M): an unbounded loop aborts with a
/// `GX0009` execution-limit error instead of hanging the tab, and `fuel_used`
/// reports how much of the budget the run consumed.
#[wasm_bindgen]
#[must_use]
pub fn run(source: &str, fuel: Option<u64>) -> JsValue {
    const DEFAULT_FUEL: u64 = 100_000_000;
    console_error_panic_hook::set_once();
    let budget = fuel.unwrap_or(DEFAULT_FUEL);
    gossamer_interp::fuel::set_fuel(budget);

    STDOUT_BUF.with(|b| b.borrow_mut().clear());
    STDERR_BUF.with(|b| b.borrow_mut().clear());

    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_pipeline(source)));

    let error = match outcome {
        Ok(Ok(())) => None,
        Ok(Err(message)) => Some(message),
        Err(payload) => Some(format!("internal error: {}", panic_payload(&payload))),
    };

    let result = RunResult {
        stdout: STDOUT_BUF.with(|b| b.borrow().clone()),
        stderr: STDERR_BUF.with(|b| b.borrow().clone()),
        error,
        fuel_used: budget.saturating_sub(gossamer_interp::fuel::fuel_remaining()),
    };
    serde_wasm_bindgen::to_value(&result).unwrap_or(JsValue::NULL)
}

/// Runs the front-end gate on `source` and returns its diagnostics as a
/// serialized `CheckResult` (`{ diagnostics: [{ severity, message,
/// line, col, code }] }`). Never executes the program.
#[wasm_bindgen]
#[must_use]
pub fn check(source: &str) -> JsValue {
    console_error_panic_hook::set_once();

    let augmented = gossamer_parse::autoderive::augment_source(source);
    let mut map = SourceMap::new();
    let file_id = map.add_file(ENTRY_NAME.to_string(), augmented.clone());
    let (diagnostics, _) = front_end(&augmented, file_id);

    let result = CheckResult {
        diagnostics: diagnostics
            .iter()
            .map(|diag| diagnostic_info(diag, &map))
            .collect(),
    };
    serde_wasm_bindgen::to_value(&result).unwrap_or(JsValue::NULL)
}

/// Full compile-and-run pipeline. Returns `Ok(())` on a clean run,
/// `Err(message)` for a front-end rejection or a runtime error /
/// panic. Output is captured through the installed writers, not the
/// return value.
fn run_pipeline(user_source: &str) -> Result<(), String> {
    // Synthesize `from_json` / `to_json` and other derives as real
    // source so the program has genuine methods, exactly as `gos run`
    // does before checking.
    let source = gossamer_parse::autoderive::augment_source(user_source);
    let mut map = SourceMap::new();
    let file_id = map.add_file(ENTRY_NAME.to_string(), source.clone());

    let (diagnostics, lowered) = front_end(&source, file_id);
    let Some((program, tcx)) = lowered else {
        let mut rendered = String::new();
        for diag in &diagnostics {
            rendered.push_str(&gossamer_diagnostics::render(
                diag,
                &map,
                RenderOptions { colour: false },
            ));
            rendered.push('\n');
        }
        capture_stderr(&rendered);
        return Err(format!(
            "{} front-end error(s); refusing to execute",
            diagnostics.len()
        ));
    };

    gossamer_interp::set_stdout_writer(capture_stdout);
    gossamer_interp::set_stderr_writer(capture_stderr);
    gossamer_interp::set_program_name(ENTRY_NAME);
    gossamer_interp::set_program_args(&[]);

    let mut vm = gossamer_interp::Vm::new();
    vm.load(&program, tcx, true)
        .map_err(|err| format!("vm load failed: {err}"))?;
    drop(program);

    let call = vm.call("main", Vec::new());
    vm.release_jit_prelude();
    gossamer_interp::join_outstanding_goroutines();
    gossamer_interp::flush_runtime_stdout();

    match call {
        Ok(_) => Ok(()),
        Err(err) if gossamer_interp::is_panic_error(&err) => {
            Err(gossamer_interp::panic_message(&err))
        }
        Err(err) => Err(format!("runtime error: {err}")),
    }
}

/// Parse + resolve + typecheck + exhaustiveness under the same fatal
/// policy as `gossamer_driver::check_frontend`, minus the on-disk
/// frontend cache (irrelevant to a one-shot wasm run). On a clean gate
/// the program is lowered to HIR and returned alongside its type
/// context; otherwise the fatal diagnostics are returned and nothing is
/// lowered.
fn front_end(
    source: &str,
    file_id: FileId,
) -> (Vec<Diagnostic>, Option<(gossamer_hir::HirProgram, TyCtxt)>) {
    let (sf, parse_diags) = gossamer_parse::autoderive::parse_with_autoderive(source, file_id);
    let mut diagnostics: Vec<Diagnostic> = parse_diags
        .iter()
        .map(gossamer_parse::ParseDiagnostic::to_diagnostic)
        .collect();

    let (resolutions, resolve_diags) = resolve_source_file(&sf);
    let in_scope = top_level_names(&sf);
    for diag in &resolve_diags {
        if matches!(
            diag.error,
            ResolveError::UnresolvedName { .. }
                | ResolveError::DuplicateItem { .. }
                | ResolveError::UnknownModulePath { .. }
        ) {
            diagnostics.push(diag.to_diagnostic(&in_scope));
        }
    }

    let mut tcx = TyCtxt::new();
    let (table, type_diags) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    diagnostics.extend(
        type_diags
            .iter()
            .map(gossamer_types::TypeDiagnostic::to_diagnostic),
    );

    let exhaustive_diags = check_exhaustiveness(&sf, &resolutions, &table, &tcx);
    for diag in &exhaustive_diags {
        if matches!(diag.error, ExhaustivenessError::NonExhaustive { .. }) {
            diagnostics.push(diag.to_diagnostic());
        }
    }

    if diagnostics.is_empty() {
        let program = gossamer_hir::lower_source_file(&sf, &resolutions, &table, &mut tcx);
        (diagnostics, Some((program, tcx)))
    } else {
        (diagnostics, None)
    }
}

/// Lowers a [`Diagnostic`] to the JS-facing shape, resolving its
/// primary label's byte span to a one-based line / column.
fn diagnostic_info(diag: &Diagnostic, map: &SourceMap) -> DiagnosticInfo {
    let (line, col) = diag
        .labels
        .iter()
        .find(|label| label.primary)
        .or_else(|| diag.labels.first())
        .map_or((0, 0), |label| {
            let position = map.line_col(label.location.file, label.location.span.start);
            (position.line, position.column)
        });

    DiagnosticInfo {
        severity: diag.severity.tag().to_string(),
        message: diag.title.clone(),
        line,
        col,
        code: diag.code.as_str().to_string(),
    }
}

/// Top-level item names, seeding the resolver's "did you mean ...?"
/// suggestions when rendering an unresolved-name diagnostic.
fn top_level_names(sf: &SourceFile) -> Vec<&str> {
    sf.items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Fn(decl) => Some(decl.name.name.as_str()),
            ItemKind::Struct(decl) => Some(decl.name.name.as_str()),
            ItemKind::Enum(decl) => Some(decl.name.name.as_str()),
            ItemKind::Trait(decl) => Some(decl.name.name.as_str()),
            ItemKind::TypeAlias(decl) => Some(decl.name.name.as_str()),
            ItemKind::Const(decl) => Some(decl.name.name.as_str()),
            ItemKind::Static(decl) => Some(decl.name.name.as_str()),
            ItemKind::Mod(decl) => Some(decl.name.name.as_str()),
            ItemKind::Impl(_) | ItemKind::AttrItem(_) => None,
        })
        .collect()
}

/// Best-effort extraction of a `catch_unwind` payload's message.
fn panic_payload(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic".to_string()
    }
}
