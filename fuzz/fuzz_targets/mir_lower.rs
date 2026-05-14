#![no_main]
//! Fuzz target: full front-end through HIR + MIR lowering, then
//! a pass through the MIR optimiser.
//!
//! Catches the class of bug where an optimisation pass corrupts
//! the body in a way the structural verifier would reject. We
//! run `verify_body` after `optimise` so a regression that lets
//! a CFG drift past the verifier surfaces as a fuzz failure.

use libfuzzer_sys::fuzz_target;

use gossamer_hir::lower_source_file;
use gossamer_lex::SourceMap;
use gossamer_mir::{lower_program, optimise, verify::verify_body};
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{TyCtxt, typecheck_source_file};

fuzz_target!(|data: &[u8]| {
    let Ok(source) = std::str::from_utf8(data) else {
        return;
    };
    if source.len() > 32 * 1024 {
        return;
    }
    let mut map = SourceMap::new();
    let file = map.add_file("fuzz.gos", source.to_string());
    let (sf, diags) = parse_source_file(source, file);
    if !diags.is_empty() {
        // Lowering is undefined on inputs that didn't parse
        // cleanly. The parse fuzz target covers that path.
        return;
    }
    let (resolutions, r_diags) = resolve_source_file(&sf);
    if !r_diags.is_empty() {
        return;
    }
    let mut tcx = TyCtxt::new();
    let (table, t_diags) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    if !t_diags.is_empty() {
        return;
    }
    let hir = lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let mut bodies = lower_program(&hir, &mut tcx);
    for body in &mut bodies {
        // `optimise` itself calls `debug_verify_body` between
        // passes under `debug_assertions`. We additionally run a
        // post-pass verify here so a release-mode fuzz run also
        // catches structural drift.
        optimise(body, &tcx);
        if verify_body(body).is_err() {
            // Verifier-rejected bodies are a real bug: the
            // optimiser should preserve structural invariants.
            // Panic so libFuzzer reports the input as a crash.
            panic!("MIR verifier rejected body `{}` after optimise()", body.name);
        }
    }
});
