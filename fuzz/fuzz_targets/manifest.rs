#![no_main]

use libfuzzer_sys::fuzz_target;

use gossamer_pkg::Manifest;

fuzz_target!(|data: &[u8]| {
    let Ok(source) = std::str::from_utf8(data) else {
        return;
    };
    if source.len() > 64 * 1024 {
        return;
    }
    let _ = Manifest::parse(source);
});
