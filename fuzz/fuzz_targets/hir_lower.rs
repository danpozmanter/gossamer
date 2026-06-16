#![no_main]
//! Fuzz target: front-end → HIR lowering only
//!
//! Complements `mir_lower` by isolating the HIR lowering pass.
//! HIR is the input MIR consumes; bugs that produce malformed
//! HIR show up downstream as cryptic MIR shape failures. This
//! target keeps the typecheck filter (HIR lowering is only
//! defined on well-typed AST) but stops before MIR.

use libfuzzer_sys::fuzz_target;

use gossamer_fuzz::grammar;
use gossamer_hir::lower_source_file;
use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{TyCtxt, typecheck_source_file};

fuzz_target!(|data: &[u8]| {
    // The symbol interner is process-global and never evicts; reset it
    // each iteration so a long fuzz run does not accumulate every random
    // identifier ever seen (otherwise RSS grows unbounded -> OOM).
    gossamer_lex::reset_interner();
    // Grammar-aware input
    let source = grammar::render_source(data);
    if source.len() > 32 * 1024 {
        return;
    }
    let mut map = SourceMap::new();
    let file = map.add_file("fuzz.gos", source.clone());
    let (sf, diags) = parse_source_file(&source, file);
    if !diags.is_empty() {
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
    let _hir = lower_source_file(&sf, &resolutions, &table, &mut tcx);
});
