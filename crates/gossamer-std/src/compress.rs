//! Runtime support for `std::compress` - compression and decompression codecs.

#![forbid(unsafe_code)]

/// Bzip2 encoder and decoder. Backed by the C `bzip2` crate, gated
/// out of the wasm playground (the pure-Rust gzip/flate/zlib codecs
/// stay available there).
#[cfg(not(target_arch = "wasm32"))]
pub mod bzip2;
/// Raw DEFLATE (RFC 1951) encoder and decoder.
pub mod flate;
/// Gzip (RFC 1952) encoder and decoder.
pub mod gzip;
/// Zlib (RFC 1950) encoder and decoder.
pub mod zlib;
/// Zstandard encoder and decoder. Backed by the C `zstd` crate, gated
/// out of the wasm playground.
#[cfg(not(target_arch = "wasm32"))]
pub mod zstd;
