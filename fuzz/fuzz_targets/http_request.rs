#![no_main]

use libfuzzer_sys::fuzz_target;

use gossamer_std::http;

fuzz_target!(|data: &[u8]| {
    let Ok(line) = std::str::from_utf8(data) else {
        return;
    };
    if line.len() > 8 * 1024 {
        return;
    }
    let _ = http::parse_request_line(line);
    let _ = http::parse_status_line(line);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = http::server::fuzz_parse_request_head(data);
});
