//! Runtime support for `std::tls` — TLS termination and dialling.
//! Backed by [`rustls`] + the Mozilla root-CA bundle from
//! [`webpki-roots`].
//!
//! Three sets of builders are exposed:
//! - [`server_config`] / [`server_config_with_client_auth`] produce a
//!   TLS-terminating `ServerConfig` from PEM-encoded certificates.
//!   The `_with_client_auth` variant turns on mutual TLS by requiring
//!   a client certificate signed by the supplied trust store.
//! - [`client_config`] / [`client_config_with_certificate`] produce
//!   client-side configurations: the bare form pins to the bundled
//!   Mozilla roots, the `_with_certificate` form additionally
//!   presents a client certificate (for mTLS) and lets callers swap
//!   in a custom root store.
//! - ALPN / SNI helpers thread through both sides.
//!
//! All handles are opaque wrappers around the underlying `rustls`
//! configs, kept behind a struct so programmatic callers can not
//! depend on the rustls version.

#![forbid(unsafe_code)]

use std::io::BufReader;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::server::WebPkiClientVerifier;
use rustls::{
    ClientConnection, RootCertStore, ServerConfig as RustlsServerConfig, ServerConnection,
};

use crate::errors::Error;

/// A PEM-encoded certificate plus its matching private key. Consumed
/// by [`server_config`] to configure a TLS-terminating listener.
#[derive(Debug, Clone)]
pub struct CertKey {
    /// PEM-encoded certificate chain (leaf first).
    pub cert_pem: Vec<u8>,
    /// PEM-encoded private key.
    pub key_pem: Vec<u8>,
}

/// Server-side TLS configuration. Clone cheaply.
#[derive(Clone)]
pub struct ServerConfig {
    inner: Arc<RustlsServerConfig>,
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("ServerConfig(...)")
    }
}

impl ServerConfig {
    /// Borrows the underlying rustls handle. Useful when wiring the
    /// config into a `rustls::ServerConnection` inside the HTTP
    /// server.
    #[must_use]
    pub fn rustls(&self) -> Arc<RustlsServerConfig> {
        Arc::clone(&self.inner)
    }

    /// Constructs a `rustls::ServerConnection` ready to be paired with
    /// an underlying TCP stream.
    pub fn new_connection(&self) -> Result<ServerConnection, Error> {
        ServerConnection::new(Arc::clone(&self.inner)).map_err(|e| wrap_err("new_connection", e))
    }
}

/// Client-side TLS configuration.
#[derive(Clone)]
pub struct ClientConfig {
    inner: Arc<rustls::ClientConfig>,
}

impl std::fmt::Debug for ClientConfig {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("ClientConfig(...)")
    }
}

impl ClientConfig {
    /// Borrows the underlying rustls handle.
    #[must_use]
    pub fn rustls(&self) -> Arc<rustls::ClientConfig> {
        Arc::clone(&self.inner)
    }

    /// Builds a TLS connection state for a SNI hostname. The hostname
    /// must be DNS-valid; IP-only servers return `Err`.
    pub fn new_connection(&self, server_name: &str) -> Result<ClientConnection, Error> {
        let name = ServerName::try_from(server_name.to_string())
            .map_err(|e| wrap_err("server name", e))?;
        ClientConnection::new(Arc::clone(&self.inner), name)
            .map_err(|e| wrap_err("new_connection", e))
    }
}

/// Produces a server-side TLS configuration from a PEM-encoded
/// certificate chain and matching private key. No client-cert
/// verification.
pub fn server_config(cert: CertKey) -> Result<ServerConfig, Error> {
    install_ring_provider();
    let certs = read_certs(&cert.cert_pem).map_err(|e| wrap_err("cert parse", e))?;
    if certs.is_empty() {
        return Err(Error::new(
            "std::tls::server_config: no certificates in PEM",
        ));
    }
    let key = read_private_key(&cert.key_pem).map_err(|e| wrap_err("key parse", e))?;
    let config = RustlsServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| wrap_err("build server config", e))?;
    Ok(ServerConfig {
        inner: Arc::new(config),
    })
}

/// Mutual-TLS server configuration: clients must present a
/// certificate signed by the supplied PEM trust bundle. Use the
/// returned config in place of [`server_config`] for service-mesh
/// or partner-channel deployments.
pub fn server_config_with_client_auth(
    cert: CertKey,
    client_ca_pem: &[u8],
) -> Result<ServerConfig, Error> {
    install_ring_provider();
    let certs = read_certs(&cert.cert_pem).map_err(|e| wrap_err("cert parse", e))?;
    if certs.is_empty() {
        return Err(Error::new(
            "std::tls::server_config_with_client_auth: no server certificates",
        ));
    }
    let key = read_private_key(&cert.key_pem).map_err(|e| wrap_err("key parse", e))?;
    let mut roots = RootCertStore::empty();
    let mut count = 0;
    for cert in read_certs(client_ca_pem).map_err(|e| wrap_err("client ca", e))? {
        roots.add(cert).map_err(|e| wrap_err("client root", e))?;
        count += 1;
    }
    if count == 0 {
        return Err(Error::new(
            "std::tls::server_config_with_client_auth: no client CAs in PEM",
        ));
    }
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| wrap_err("client verifier", e))?;
    let config = RustlsServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)
        .map_err(|e| wrap_err("build server config", e))?;
    Ok(ServerConfig {
        inner: Arc::new(config),
    })
}

/// Sets the ALPN protocol list negotiated with each connecting
/// client. Standard values: `b"h2"`, `b"http/1.1"`. Returns a fresh
/// [`ServerConfig`] — the input is not mutated.
#[must_use]
pub fn server_with_alpn(config: ServerConfig, protocols: &[&[u8]]) -> ServerConfig {
    let mut inner = (*config.inner).clone();
    inner.alpn_protocols = protocols.iter().map(|p| p.to_vec()).collect();
    ServerConfig {
        inner: Arc::new(inner),
    }
}

/// Produces a client-side TLS configuration pinned to the bundled
/// Mozilla root certificate store.
pub fn client_config() -> Result<ClientConfig, Error> {
    install_ring_provider();
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(ClientConfig {
        inner: Arc::new(config),
    })
}

/// Client-side mTLS configuration: roots determine which servers we
/// trust, `cert` is the client identity. Pass `None` for `extra_roots_pem`
/// to use the Mozilla bundle.
pub fn client_config_with_certificate(
    cert: CertKey,
    extra_roots_pem: Option<&[u8]>,
) -> Result<ClientConfig, Error> {
    install_ring_provider();
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(pem) = extra_roots_pem {
        for cert in read_certs(pem).map_err(|e| wrap_err("extra roots", e))? {
            roots.add(cert).map_err(|e| wrap_err("extra root", e))?;
        }
    }
    let certs = read_certs(&cert.cert_pem).map_err(|e| wrap_err("client cert parse", e))?;
    if certs.is_empty() {
        return Err(Error::new(
            "std::tls::client_config_with_certificate: no client cert in PEM",
        ));
    }
    let key = read_private_key(&cert.key_pem).map_err(|e| wrap_err("client key parse", e))?;
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(certs, key)
        .map_err(|e| wrap_err("build client config", e))?;
    Ok(ClientConfig {
        inner: Arc::new(config),
    })
}

/// Adds an ALPN protocol list to a client config.
#[must_use]
pub fn client_with_alpn(config: ClientConfig, protocols: &[&[u8]]) -> ClientConfig {
    let mut inner = (*config.inner).clone();
    inner.alpn_protocols = protocols.iter().map(|p| p.to_vec()).collect();
    ClientConfig {
        inner: Arc::new(inner),
    }
}

fn install_ring_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn wrap_err(context: &str, error: impl std::fmt::Display) -> Error {
    Error::new(format!("std::tls: {context}: {error}"))
}

fn read_certs(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, std::io::Error> {
    let mut reader = BufReader::new(pem);
    rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>()
}

fn read_private_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>, std::io::Error> {
    let mut reader = BufReader::new(pem);
    for item in rustls_pemfile::read_all(&mut reader) {
        match item? {
            rustls_pemfile::Item::Pkcs1Key(k) => return Ok(PrivateKeyDer::Pkcs1(k)),
            rustls_pemfile::Item::Pkcs8Key(k) => return Ok(PrivateKeyDer::Pkcs8(k)),
            rustls_pemfile::Item::Sec1Key(k) => return Ok(PrivateKeyDer::Sec1(k)),
            _ => {}
        }
    }
    Err(std::io::Error::other("no private key found in PEM"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_config_builds_against_mozilla_roots() {
        let config = client_config().expect("client config");
        assert!(!format!("{config:?}").is_empty());
    }

    #[test]
    fn server_config_rejects_empty_pem() {
        let err = server_config(CertKey {
            cert_pem: Vec::new(),
            key_pem: Vec::new(),
        })
        .unwrap_err();
        assert!(err.message().contains("no certificates"));
    }

    #[test]
    fn client_with_alpn_sets_protocols() {
        let cfg = client_config().unwrap();
        let with_alpn = client_with_alpn(cfg, &[b"h2", b"http/1.1"]);
        assert_eq!(with_alpn.inner.alpn_protocols.len(), 2);
        assert_eq!(with_alpn.inner.alpn_protocols[0], b"h2".to_vec());
    }
}
