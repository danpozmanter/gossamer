//! WebSocket (RFC 6455) - first-party stdlib support.
//!
//! Per the user requirement, websockets are part of the standard
//! library, not feature-gated, not an external crate.
//!
//! The frame codec (`WebSocket`, `Message`, the accept-key derivation)
//! lives in the dependency-light `gossamer-ws` crate so every execution
//! tier - the bytecode VM, the Cranelift JIT, and the LLVM AOT runtime -
//! shares one framing engine and the wire behaviour is identical. This
//! module keeps the http-dependent server-side handshake ([`accept`]),
//! which turns an incoming [`Request`] into the `101 Switching
//! Protocols` [`Response`], and re-exports the codec so the historical
//! `gossamer_std::http_websocket::{WebSocket, Message}` path keeps
//! working.

use crate::http::{Headers, Request, Response, StatusCode};

pub use gossamer_ws::{Error, Message, WebSocket, compute_accept};

/// Result of [`accept`] - the upgrade response that the caller MUST
/// write to the wire before constructing a [`WebSocket`].
#[derive(Debug)]
pub struct Upgrade {
    /// `101 Switching Protocols` response with the negotiated
    /// `Sec-WebSocket-Accept` token.
    pub response: Response,
}

/// Performs the server-side handshake from an HTTP request.
///
/// Validates `Upgrade: websocket`, `Connection: Upgrade`,
/// `Sec-WebSocket-Version: 13`, and computes `Sec-WebSocket-Accept` per
/// RFC 6455 §4.2.2.
///
/// After this call returns Ok, the caller writes the `upgrade.response`
/// to the wire and then wraps the connection in a [`WebSocket`].
pub fn accept(request: &Request) -> Result<Upgrade, Error> {
    let upgrade = request
        .headers
        .get("upgrade")
        .ok_or_else(|| Error::Handshake("missing Upgrade header".into()))?;
    if !upgrade.eq_ignore_ascii_case("websocket") {
        return Err(Error::Handshake(format!("bad Upgrade: {upgrade}")));
    }
    let connection = request
        .headers
        .get("connection")
        .ok_or_else(|| Error::Handshake("missing Connection header".into()))?;
    let has_upgrade_token = connection
        .split(',')
        .any(|tok| tok.trim().eq_ignore_ascii_case("upgrade"));
    if !has_upgrade_token {
        return Err(Error::Handshake(format!("bad Connection: {connection}")));
    }
    let version = request.headers.get("sec-websocket-version").unwrap_or("");
    if version.trim() != "13" {
        return Err(Error::Handshake(format!("bad version: {version}")));
    }
    let key = request
        .headers
        .get("sec-websocket-key")
        .ok_or_else(|| Error::Handshake("missing Sec-WebSocket-Key".into()))?;
    let accept_token = compute_accept(key);
    let mut headers = Headers::new();
    headers.insert("upgrade", "websocket");
    headers.insert("connection", "Upgrade");
    headers.insert("sec-websocket-accept", &accept_token);
    Ok(Upgrade {
        response: Response {
            status: StatusCode(101),
            headers,
            body: Vec::new(),
            raw_header_pairs: Vec::new(),
            body_stream: None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::http::Method;

    fn make_request(version: &str, key: &str, upgrade: &str, conn: &str) -> Request {
        let mut h = Headers::new();
        h.insert("host", "localhost");
        h.insert("upgrade", upgrade);
        h.insert("connection", conn);
        h.insert("sec-websocket-version", version);
        h.insert("sec-websocket-key", key);
        Request {
            method: Method::Get,
            path: "/ws".into(),
            query: String::new(),
            headers: h,
            body: Vec::new(),
            context: Context::background(),
            trailers: None,
        }
    }

    #[test]
    fn accept_builds_101_response() {
        let req = make_request("13", "dGhlIHNhbXBsZSBub25jZQ==", "websocket", "Upgrade");
        let up = accept(&req).expect("upgrade");
        assert_eq!(up.response.status, StatusCode(101));
        assert_eq!(
            up.response.headers.get("sec-websocket-accept"),
            Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
        );
        assert_eq!(
            up.response
                .headers
                .get("upgrade")
                .map(str::to_ascii_lowercase),
            Some("websocket".into())
        );
    }

    #[test]
    fn accept_rejects_wrong_version() {
        let req = make_request("8", "x", "websocket", "Upgrade");
        let err = accept(&req).unwrap_err();
        assert!(matches!(err, Error::Handshake(_)));
    }

    #[test]
    fn accept_rejects_missing_upgrade_token() {
        let req = make_request("13", "x", "ws", "Upgrade");
        let err = accept(&req).unwrap_err();
        assert!(matches!(err, Error::Handshake(_)));
    }

    #[test]
    fn accept_rejects_missing_connection_upgrade() {
        let req = make_request("13", "x", "websocket", "keep-alive");
        let err = accept(&req).unwrap_err();
        assert!(matches!(err, Error::Handshake(_)));
    }
}
