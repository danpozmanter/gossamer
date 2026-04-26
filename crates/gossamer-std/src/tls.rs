//! Runtime support for `std::tls` — TLS termination and dialling.
//! Backed by [`rustls`] + the Mozilla root-CA bundle from
//! [`webpki-roots`]. Two builders land in the first slice:
//! - [`server_config`] produces a TLS-terminating `ServerConfig`
//!   from a PEM-encoded certificate chain + private key.
//! - [`client_config`] produces a TLS-dialling `ClientConfig` pinned
//!   to the bundled root CAs.
//!
//! Both handles are opaque wrappers around the underlying `rustls`
//! config, kept behind an opaque struct so programmatic callers can
//! not depend on the rustls version.

#![forbid(unsafe_code)]

use std::io::BufReader;
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{RootCertStore, ServerConfig as RustlsServerConfig};

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
}

/// Produces a server-side TLS configuration from a PEM-encoded
/// certificate chain and matching private key.
pub fn server_config(cert: CertKey) -> Result<ServerConfig, Error> {
    install_ring_provider();
    let certs = read_certs(&cert.cert_pem).map_err(|e| wrap_err("cert parse", e))?;
    if certs.is_empty() {
        return Err(Error::new("std::tls::server_config: no certificates in PEM"));
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
        // Opaque handle — just assert we got a usable Arc.
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
}
