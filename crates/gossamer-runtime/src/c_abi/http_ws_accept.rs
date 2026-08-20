#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::similar_names)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::wildcard_imports)]

use super::*;

// ---------------------------------------------------------------
// websocket::accept(request) -> Result<Response, errors::Error>
//
// Compiled-tier entry for the RFC 6455 server-side handshake.
// Validates the upgrade headers, computes the Sec-WebSocket-Accept
// token (reusing the already-wired `gos_rt_ws_accept_key` math), and
// returns a 101 Switching Protocols Response carrying the negotiated
// headers. Mirrors `gossamer_std::http_websocket::accept` exactly so
// the interp `websocket::accept` native and this shim emit
// bit-identical results (status, headers, and error strings) across
// VM / Cranelift / LLVM.
//
// The packed `i128` Result follows the same convention as
// `gos_rt_http_get`: discriminant 0 = Ok(*mut GosHttpResponse),
// 1 = Err(*mut GosError). The Ok payload is field-compatible with a
// client Response so callers read `resp.status` / `resp.headers`
// through the same projections.
// ---------------------------------------------------------------

/// Case-insensitive header lookup over the server-populated
/// `request.headers` (lowercased, merged by the h1 parser).
fn header_lookup(req: &GosHttpRequest, name: &str) -> Option<String> {
    req.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

/// Packs an `Err(errors::Error)` Result with the given message,
/// matching the interp tier's handshake error strings.
fn handshake_err(msg: &str) -> i128 {
    let err = crate::c_abi::errors::error_new_from_bytes(msg.as_bytes());
    crate::c_abi::vec::pack_result(1, err as i64)
}

/// `http::websocket::is_websocket_upgrade(request) -> bool`. True when
/// the request carries `Upgrade: websocket` and a `Connection` header
/// whose value contains the `upgrade` token. Mirrors the interp
/// `builtin_ws_is_upgrade` so both tiers classify identically.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_ws_is_upgrade(req: *const GosHttpRequest) -> i64 {
    ffi_entry!(0, {
        if req.is_null() {
            return 0;
        }
        let request = unsafe { &*req };
        let mut has_upgrade_ws = false;
        let mut has_connection_upgrade = false;
        for (name, value) in &request.headers {
            let n = name.to_ascii_lowercase();
            let v = value.to_ascii_lowercase();
            if n == "upgrade" && v == "websocket" {
                has_upgrade_ws = true;
            }
            if n == "connection" && v.contains("upgrade") {
                has_connection_upgrade = true;
            }
        }
        i64::from(has_upgrade_ws && has_connection_upgrade)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_ws_accept(req: *const GosHttpRequest) -> i128 {
    ffi_entry!(0i128, {
        if req.is_null() {
            return handshake_err("missing Upgrade header");
        }
        let request = unsafe { &*req };

        // 1. Upgrade: websocket
        let Some(upgrade) = header_lookup(request, "upgrade") else {
            return handshake_err("missing Upgrade header");
        };
        if !upgrade.eq_ignore_ascii_case("websocket") {
            return handshake_err(&format!("bad Upgrade: {upgrade}"));
        }
        // 2. Connection: (contains) upgrade
        let Some(connection) = header_lookup(request, "connection") else {
            return handshake_err("missing Connection header");
        };
        let has_upgrade_token = connection
            .split(',')
            .any(|tok| tok.trim().eq_ignore_ascii_case("upgrade"));
        if !has_upgrade_token {
            return handshake_err(&format!("bad Connection: {connection}"));
        }
        // 3. Sec-WebSocket-Version: 13
        let version = header_lookup(request, "sec-websocket-version").unwrap_or_default();
        if version.trim() != "13" {
            return handshake_err(&format!("bad version: {version}"));
        }
        // 4. Sec-WebSocket-Key present.
        let Some(key) = header_lookup(request, "sec-websocket-key") else {
            return handshake_err("missing Sec-WebSocket-Key");
        };

        // Token: reuse the wired accept-key derivation (base64(sha1(key
        // + GUID))) so this shim and `gos_rt_ws_accept_key` never drift.
        let key_c = std::ffi::CString::new(key.as_str())
            .unwrap_or_else(|_| std::ffi::CString::new("").expect("static is NUL-free"));
        let token_ptr = unsafe { gos_rt_ws_accept_key(key_c.as_ptr()) };
        let token = if token_ptr.is_null() {
            String::new()
        } else {
            unsafe { crate::c_abi::gos_str_arg_string(token_ptr) }
        };

        let headers: Vec<(String, String)> = vec![
            ("upgrade".to_string(), "websocket".to_string()),
            ("connection".to_string(), "Upgrade".to_string()),
            ("sec-websocket-accept".to_string(), token),
        ];
        let resp = Box::into_raw(Box::new(GosHttpResponse {
            status: 101,
            body: SyncRawPtr::new(alloc_cstring(b"")),
            headers,
            body_bytes: Some(Vec::new()),
            content_type: String::new(),
            stream_handle: -1,
        }));
        crate::c_abi::vec::pack_result(0, resp as i64)
    })
}
