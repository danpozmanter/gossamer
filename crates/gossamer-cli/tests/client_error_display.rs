//! Cross-crate lock on the HTTP client error message contract.
//!
//! The interp tier surfaces failures through
//! `gossamer_std::http::ClientError`'s `Display`; the compiled
//! tiers hand-build the same strings inline in
//! `gossamer-runtime/src/c_abi/http_client.rs` (`format!("http:
//! transport: {e}")` / `"http: io: {e}"`). The two halves are not
//! linked by a shared constant, so this test pins the std side's
//! rendered shape - the runtime side is locked by
//! `http_request_send_transport_failure_packs_interp_shaped_err`
//! in the runtime crate, which asserts the identical
//! `"http: transport:"` prefix off a live refused connection.

#![allow(missing_docs)]

use gossamer_std::http::ClientError;

#[test]
fn transport_error_renders_canonical_prefix() {
    assert_eq!(
        ClientError::Transport("x".to_string()).to_string(),
        "http: transport: x",
    );
}

#[test]
fn io_error_renders_canonical_prefix() {
    assert_eq!(ClientError::Io("x".to_string()).to_string(), "http: io: x");
}

#[test]
fn cancelled_error_renders_fixed_message() {
    assert_eq!(
        ClientError::Cancelled.to_string(),
        "http: cancelled by context"
    );
}

#[test]
fn unknown_method_error_text_matches_runtime_label() {
    // `Client::request("FROBNICATE", …)` and the runtime's
    // `validate_http_method` both name the method; the std side
    // wraps it in the transport class.
    let err = gossamer_std::http::Client::new()
        .request("FROBNICATE", "http://127.0.0.1:1/never", None, &[])
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "http: transport: unsupported HTTP method: FROBNICATE",
    );
}
