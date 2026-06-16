//! Resolver-only fuzz target
//!
//! The existing `typecheck` / `mir_lower` / `vm_compile` targets
//! all short-circuit on a parse error, so the resolver only ever
//! sees parse-clean input. This target keeps the parse, but always
//! invokes the resolver - even on parse-with-errors input - so
//! shapes like cyclic `use` declarations, glob expansion edges,
//! and `mod` re-entry that the parser tolerates but the resolver
//! must reject get exercised.

#![no_main]

use libfuzzer_sys::fuzz_target;

use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;

fuzz_target!(|data: &[u8]| {
    // The symbol interner is process-global and never evicts; reset it
    // each iteration so a long fuzz run does not accumulate every random
    // identifier ever seen (otherwise RSS grows unbounded -> OOM).
    gossamer_lex::reset_interner();
    let Ok(source) = std::str::from_utf8(data) else {
        return;
    };
    if source.len() > 64 * 1024 {
        return;
    }
    let mut map = SourceMap::new();
    let file = map.add_file("fuzz.gos", source.to_string());
    let (sf, _diags) = parse_source_file(source, file);
    // Always resolve - even when parsing produced diagnostics.
    // The resolver must not panic on the AST it gets back.
    let _ = resolve_source_file(&sf);
});
