//! Runtime support for `std::crypto`.
//! Scoped to widely-needed primitives: secure random bytes, SHA-256
//! via the existing pure-Rust helper, HMAC-SHA-256, and a
//! constant-time compare. A full TLS stack arrives with `std::tls`
//! in a later phase — the plan's exit criterion is that it be
//! "carefully reviewed" rather than "quick".

#![forbid(unsafe_code)]

pub mod rand {
    //! OS-backed secure random bytes.

    use crate::errors::Error;

    /// Fills `buf` with cryptographically-secure random bytes from
    /// the host's CSPRNG.
    ///
    /// Uses the `getrandom` crate, which routes to the kernel CSPRNG
    /// on every supported target: `getrandom(2)` on Linux, `arc4random`
    /// on macOS / *BSD, and `BCryptGenRandom` on Windows. Preserves
    /// the workspace's no-`unsafe` rule because all platform FFI is
    /// contained inside the `getrandom` crate.
    pub fn fill(buf: &mut [u8]) -> Result<(), Error> {
        getrandom::getrandom(buf).map_err(|e| Error::new(format!("rand: {e}")))
    }

    /// Convenience: allocates a fresh buffer and returns `n` random
    /// bytes.
    pub fn bytes(n: usize) -> Result<Vec<u8>, Error> {
        let mut out = vec![0u8; n];
        fill(&mut out)?;
        Ok(out)
    }
}

pub mod sha256 {
    //! SHA-256 hashing — thin wrapper over the in-repo `gossamer-pkg`
    //! implementation so the crypto module has a stable, pure-Rust
    //! digest without pulling in a crypto crate.

    /// Returns the 32-byte SHA-256 digest of `input`.
    #[must_use]
    pub fn digest(input: &[u8]) -> [u8; 32] {
        gossamer_pkg::sha256::digest(input)
    }

    /// Returns the SHA-256 digest as lowercase hex.
    #[must_use]
    pub fn hex(input: &[u8]) -> String {
        gossamer_pkg::sha256::hex(input)
    }
}

pub mod hmac {
    //! HMAC-SHA-256.

    use super::sha256;

    const BLOCK_SIZE: usize = 64;

    /// HMAC-SHA-256 keyed MAC over `message`.
    #[must_use]
    pub fn sha256_mac(key: &[u8], message: &[u8]) -> [u8; 32] {
        let mut block = [0u8; BLOCK_SIZE];
        if key.len() > BLOCK_SIZE {
            block[..32].copy_from_slice(&sha256::digest(key));
        } else {
            block[..key.len()].copy_from_slice(key);
        }
        let mut inner_key = [0u8; BLOCK_SIZE];
        let mut outer_key = [0u8; BLOCK_SIZE];
        for i in 0..BLOCK_SIZE {
            inner_key[i] = block[i] ^ 0x36;
            outer_key[i] = block[i] ^ 0x5c;
        }
        let mut inner_input = Vec::with_capacity(BLOCK_SIZE + message.len());
        inner_input.extend_from_slice(&inner_key);
        inner_input.extend_from_slice(message);
        let inner_hash = sha256::digest(&inner_input);
        let mut outer_input = Vec::with_capacity(BLOCK_SIZE + 32);
        outer_input.extend_from_slice(&outer_key);
        outer_input.extend_from_slice(&inner_hash);
        sha256::digest(&outer_input)
    }
}

pub mod subtle {
    //! Constant-time compare helpers. Hides operand-dependent
    //! branches so attackers cannot time-side-channel on
    //! byte-by-byte secret comparison.

    /// Returns `true` iff `a == b`, with running time independent of
    /// where the first byte difference occurs.
    #[must_use]
    pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        let mut diff: u8 = 0;
        for i in 0..a.len() {
            diff |= a[i] ^ b[i];
        }
        diff == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256::hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hmac_sha256_matches_rfc_vector() {
        // RFC 4231 test case 1: key = 20 * 0x0b, data = "Hi There".
        let key = [0x0bu8; 20];
        let mac = hmac::sha256_mac(&key, b"Hi There");
        let hex = super::sha256::hex(&[]);
        let _ = hex;
        assert_eq!(
            to_hex(&mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn constant_time_eq_matches_std_eq() {
        assert!(subtle::constant_time_eq(b"same", b"same"));
        assert!(!subtle::constant_time_eq(b"same", b"samd"));
        assert!(!subtle::constant_time_eq(b"same", b"longer"));
    }

    #[test]
    fn rand_fill_returns_non_constant_bytes() {
        let a = rand::bytes(32).unwrap();
        let b = rand::bytes(32).unwrap();
        assert_ne!(a, b);
    }

    fn to_hex(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push(nibble(byte >> 4));
            out.push(nibble(byte & 0xf));
        }
        out
    }

    fn nibble(n: u8) -> char {
        match n {
            0..=9 => (b'0' + n) as char,
            10..=15 => (b'a' + n - 10) as char,
            _ => '?',
        }
    }
}
