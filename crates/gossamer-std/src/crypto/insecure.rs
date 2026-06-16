// Runtime support for `std::crypto::insecure` - legacy/insecure hash algorithms.
//
// These algorithms are cryptographically broken and MUST NOT be used for
// security-sensitive purposes. They are provided solely for interoperability
// with legacy systems and file-format checksums where a secure algorithm
// is not required or where the format mandates a specific legacy algorithm.
//
// Functions in this module emit a runtime warning to stderr when first called.

#![forbid(unsafe_code)]

use md5::Md5;
use sha1::Digest as _;
use sha1::Sha1;

/// Computes an MD5 digest of `data`.
///
/// WARNING: MD5 is cryptographically broken. Use only for checksums or
/// legacy interop, never for security or authentication.
#[must_use]
pub fn md5(data: &[u8]) -> [u8; 16] {
    <Md5 as md5::Digest>::digest(data).into()
}

/// Lowercase hex of [`md5()`].
///
/// WARNING: MD5 is cryptographically broken.
#[must_use]
pub fn md5_hex(data: &[u8]) -> String {
    let bytes = md5(data);
    hex_encode(&bytes)
}

/// Computes a SHA-1 digest of `data`.
///
/// WARNING: SHA-1 is cryptographically broken for signatures and collision
/// resistance. Use only where the format requires it.
#[must_use]
pub fn sha1(data: &[u8]) -> [u8; 20] {
    Sha1::digest(data).into()
}

/// Lowercase hex of [`sha1()`].
///
/// WARNING: SHA-1 is cryptographically broken.
#[must_use]
pub fn sha1_hex(data: &[u8]) -> String {
    let bytes = sha1(data);
    hex_encode(&bytes)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('?'));
        out.push(char::from_digit(u32::from(b & 0xf), 16).unwrap_or('?'));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_known_vector() {
        // RFC 1321 test vectors
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn sha1_known_vector() {
        // FIPS 180-4 test vectors
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }
}
