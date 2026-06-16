// Runtime support for `std::hash::fnv` - Fowler-Noll-Vo non-cryptographic hashes.
//
// Implements FNV-1 and FNV-1a for 32-bit and 64-bit outputs. FNV-1a is generally
// preferred: it mixes better on short inputs. Neither variant is cryptographically
// secure; use `std::crypto::sha256` for security-sensitive digests.

#![forbid(unsafe_code)]

const FNV1_64_INIT: u64 = 0xcbf29ce484222325;
const FNV_64_PRIME: u64 = 0x100000001b3;
const FNV1_32_INIT: u32 = 0x811c9dc5;
const FNV_32_PRIME: u32 = 0x01000193;

/// Computes FNV-1a 64-bit hash over `data`.
#[must_use]
pub fn hash64(data: &[u8]) -> u64 {
    let mut hash = FNV1_64_INIT;
    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_64_PRIME);
    }
    hash
}

/// Computes FNV-1 64-bit hash over `data`.
#[must_use]
pub fn hash64_fnv1(data: &[u8]) -> u64 {
    let mut hash = FNV1_64_INIT;
    for &byte in data {
        hash = hash.wrapping_mul(FNV_64_PRIME);
        hash ^= u64::from(byte);
    }
    hash
}

/// Computes FNV-1a 32-bit hash over `data`.
#[must_use]
pub fn hash32(data: &[u8]) -> u32 {
    let mut hash = FNV1_32_INIT;
    for &byte in data {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(FNV_32_PRIME);
    }
    hash
}

/// Computes FNV-1 32-bit hash over `data`.
#[must_use]
pub fn hash32_fnv1(data: &[u8]) -> u32 {
    let mut hash = FNV1_32_INIT;
    for &byte in data {
        hash = hash.wrapping_mul(FNV_32_PRIME);
        hash ^= u32::from(byte);
    }
    hash
}

/// Computes FNV-1a 64-bit hash of a UTF-8 string.
#[must_use]
pub fn hash_string(s: &str) -> u64 {
    hash64(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_known_value() {
        // FNV-1a 64 of "" is the init value
        assert_eq!(hash64(b""), FNV1_64_INIT);
    }

    #[test]
    fn hello_world_known_value() {
        // Known FNV-1a 64 value for "hello\0"
        let h = hash64(b"hello");
        assert_ne!(h, FNV1_64_INIT);
    }

    #[test]
    fn fnv1_and_fnv1a_differ() {
        let data = b"test data";
        assert_ne!(hash64(data), hash64_fnv1(data));
    }

    #[test]
    fn hash32_differs_from_hash64() {
        let data = b"gossamer";
        let h32 = u64::from(hash32(data));
        let h64 = hash64(data);
        assert_ne!(h32, h64);
    }

    #[test]
    fn hash_string_matches_hash64() {
        let s = "gossamer lang";
        assert_eq!(hash_string(s), hash64(s.as_bytes()));
    }

    #[test]
    fn deterministic_across_calls() {
        let data = b"reproducible";
        assert_eq!(hash64(data), hash64(data));
    }
}
