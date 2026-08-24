//! Pins the constants the autoderive stdlib wrappers spell out in Gossamer
//! source against the Rust values they mirror. A wrapper is source text, so
//! nothing in the compiler notices when the runtime's own default moves.

#![allow(missing_docs)]

#[test]
fn http2_config_defaults_match_the_runtime() {
    let config = gossamer_std::http_h2::Config::default();
    let expected = [
        (
            "max_concurrent_streams",
            i64::from(config.max_concurrent_streams),
        ),
        ("initial_window_size", i64::from(config.initial_window_size)),
        (
            "initial_connection_window_size",
            i64::from(config.initial_connection_window_size),
        ),
        ("max_frame_size", i64::from(config.max_frame_size)),
        (
            "max_header_list_size",
            i64::from(config.max_header_list_size),
        ),
    ];
    let source = gossamer_parse::autoderive::stdlib_wrapper_source("Http2Config");
    for (field, value) in expected {
        let needle = format!("{field}: {value}");
        assert!(
            source.contains(&needle),
            "the injected Http2Config wrapper does not spell `{needle}`; the \
             runtime default moved and the Gossamer source in \
             crates/gossamer-parse/src/autoderive/stdlib_wrappers.rs must \
             follow it.\n{source}"
        );
    }
}
