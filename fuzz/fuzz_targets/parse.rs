#![no_main]

use libfuzzer_sys::fuzz_target;

use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;

fuzz_target!(|data: &[u8]| {
    // The lexer has no global reset: a future session-owned interner will
    // bound long-lived process memory without invalidating live symbols.
    let Ok(source) = std::str::from_utf8(data) else {
        return;
    };
    if source.len() > 64 * 1024 {
        return;
    }
    let mut map = SourceMap::new();
    let file = map.add_file("fuzz.gos", source.to_string());
    let (_sf, _diags) = parse_source_file(source, file);
});
