#![no_main]

use libfuzzer_sys::fuzz_target;

use gossamer_lex::{SourceMap, tokenize};

fuzz_target!(|data: &[u8]| {
    // The symbol interner is process-global and never evicts; reset it
    // each iteration so a long fuzz run does not accumulate every random
    // identifier ever seen (otherwise RSS grows unbounded -> OOM).
    gossamer_lex::reset_interner();
    let Ok(source) = std::str::from_utf8(data) else {
        return;
    };
    let mut map = SourceMap::new();
    let file = map.add_file("fuzz.gos", source.to_string());
    let (_tokens, _errors) = tokenize(source, file);
});
