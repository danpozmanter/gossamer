#![no_main]

use libfuzzer_sys::fuzz_target;

use gossamer_lex::{SourceMap, tokenize};

fuzz_target!(|data: &[u8]| {
    // The lexer remains unsafe-free during fuzzing. A future session-owned
    // interner will bound long-lived compiler process RSS without exposing a
    // global reset that can invalidate live symbols.
    let Ok(source) = std::str::from_utf8(data) else {
        return;
    };
    let mut map = SourceMap::new();
    let file = map.add_file("fuzz.gos", source.to_string());
    let (_tokens, _errors) = tokenize(source, file);
});
