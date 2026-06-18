//! HTTP/3 server and client (RFC 9114) - first-party stdlib
//! support. The QUIC + h3 engine lives in the standalone
//! [`gossamer_http3`] crate so both this interpreter-facing adapter
//! and the compiled-tier runtime share one implementation; this
//! module re-exports the engine through the same `http::Request` /
//! `http::Response` types the rest of the HTTP stack speaks.
//!
//! Public surface mirrors [`crate::http_h2`]:
//!
//! - [`serve`] - bind a UDP socket, run a quinn endpoint, dispatch
//!   every accepted request to a [`crate::http::Handler`].
//! - [`Client`] - issue HTTP/3 requests against a remote endpoint.

#![allow(
    clippy::missing_errors_doc,
    clippy::module_name_repetitions,
    clippy::needless_pass_by_value
)]

use crate::errors::Error;
use crate::http::{Headers, Method, Request, Response, StatusCode};

pub use gossamer_http3::H3Error;

impl From<H3Error> for Error {
    fn from(err: H3Error) -> Self {
        Self::new(err.to_string())
    }
}

/// Handler signature for HTTP/3: receive a [`Request`], return a
/// complete [`Response`]. Mirrors [`crate::http_h2::Handler`] so the
/// two stacks share a single handler trait shape - a production
/// service can be served over h2 or h3 from the same handler value.
pub trait Handler: Send + Sync + 'static {
    /// Serves one HTTP/3 request.
    fn serve(&self, request: Request) -> Response;
}

impl<F> Handler for F
where
    F: Fn(Request) -> Response + Send + Sync + 'static,
{
    fn serve(&self, request: Request) -> Response {
        self(request)
    }
}

/// Builds a [`Request`] from the engine's wire-shaped request.
fn request_from_wire(req: gossamer_http3::H3Request) -> Request {
    let method = Method::parse(&req.method).unwrap_or(Method::Get);
    let mut headers = Headers::new();
    for (name, value) in &req.headers {
        headers.insert(name, value);
    }
    Request {
        method,
        path: req.path,
        query: req.query,
        headers,
        body: req.body,
        context: crate::context::Context::background(),
        trailers: None,
    }
}

/// Lowers a [`Response`] to the engine's wire-shaped response.
fn response_to_wire(response: Response) -> gossamer_http3::H3Response {
    let headers = response
        .headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    gossamer_http3::H3Response {
        status: response.status.as_u16(),
        headers,
        body: response.body,
    }
}

/// Binds a UDP socket on `addr`, runs a QUIC + HTTP/3 endpoint, and
/// dispatches every accepted request to `handler`. Blocks the
/// calling thread until the endpoint stops accepting.
///
/// `cert_path` / `key_path` point to PEM-encoded files. HTTP/3
/// mandates TLS, so a keypair is required.
pub fn serve<H>(addr: &str, cert_path: &str, key_path: &str, handler: H) -> Result<(), Error>
where
    H: Handler + Clone + 'static,
{
    gossamer_http3::serve_files(addr, cert_path, key_path, move |wire| {
        response_to_wire(handler.serve(request_from_wire(wire)))
    })
    .map_err(Error::from)
}

/// HTTP/3 client. Wraps the [`gossamer_http3::Client`] and presents
/// the same verb surface as [`crate::http::Client`].
#[derive(Clone)]
pub struct Client {
    inner: gossamer_http3::Client,
}

impl Client {
    /// Constructs a client that validates server certificates
    /// against the bundled Mozilla root store.
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            inner: gossamer_http3::Client::new().map_err(Error::from)?,
        })
    }

    /// Constructs a client that accepts any server certificate.
    /// Intended for tests and self-signed development endpoints -
    /// never use in production.
    pub fn insecure() -> Result<Self, Error> {
        Ok(Self {
            inner: gossamer_http3::Client::insecure().map_err(Error::from)?,
        })
    }

    /// Issues a GET request against `url`.
    pub fn get(&self, url: &str) -> Result<Response, Error> {
        self.do_request(Method::Get, url, None, &[])
    }

    /// Issues a POST request with the supplied body.
    pub fn post(&self, url: &str, body: &[u8], content_type: &str) -> Result<Response, Error> {
        self.do_request(
            Method::Post,
            url,
            Some(body),
            &[("Content-Type", content_type)],
        )
    }

    /// Issues a PUT request with the supplied body.
    pub fn put(&self, url: &str, body: &[u8], content_type: &str) -> Result<Response, Error> {
        self.do_request(
            Method::Put,
            url,
            Some(body),
            &[("Content-Type", content_type)],
        )
    }

    /// Issues a DELETE request with an optional body.
    pub fn delete(
        &self,
        url: &str,
        body: Option<&[u8]>,
        headers: &[(&str, &str)],
    ) -> Result<Response, Error> {
        self.do_request(Method::Delete, url, body, headers)
    }

    /// Issues a HEAD request. The response body is always empty per
    /// RFC 9110.
    pub fn head(&self, url: &str, headers: &[(&str, &str)]) -> Result<Response, Error> {
        self.do_request(Method::Head, url, None, headers)
    }

    /// Issues an OPTIONS request.
    pub fn options(&self, url: &str, headers: &[(&str, &str)]) -> Result<Response, Error> {
        self.do_request(Method::Options, url, None, headers)
    }

    /// Issues a request with the supplied method, optional body, and
    /// extra headers - the synchronous entry point underlying the
    /// verb helpers.
    pub fn do_request(
        &self,
        method: Method,
        url: &str,
        body: Option<&[u8]>,
        headers: &[(&str, &str)],
    ) -> Result<Response, Error> {
        let owned: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        let resp = self
            .inner
            .request(method.as_str(), url, body, &owned)
            .map_err(Error::from)?;
        let mut out_headers = Headers::new();
        for (name, value) in &resp.headers {
            out_headers.insert(name, value);
        }
        Ok(Response {
            status: StatusCode(resp.status),
            headers: out_headers,
            body: resp.body,
            raw_header_pairs: Vec::new(),
            body_stream: None,
        })
    }

    /// Issues a request whose method is given as a string. Accepts
    /// the standard verbs (case-insensitive).
    pub fn request(
        &self,
        method: &str,
        url: &str,
        body: Option<&[u8]>,
        headers: &[(&str, &str)],
    ) -> Result<Response, Error> {
        let m = Method::parse(method).ok_or_else(|| {
            Error::from(H3Error::Protocol(format!("unsupported method: {method}")))
        })?;
        self.do_request(m, url, body, headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h3_error_renders_into_std_error() {
        let e: Error = H3Error::Protocol("boom".into()).into();
        assert!(e.message().contains("boom"));
    }

    #[test]
    fn h3_client_insecure_builds() {
        let client = Client::insecure().expect("client");
        drop(client);
    }
}
