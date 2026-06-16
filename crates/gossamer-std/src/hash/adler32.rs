// Runtime support for `std::hash::adler32` - Adler-32 checksums.
//
// Adler-32 is used in zlib (RFC 1950) streams. It is faster than CRC-32
// but offers weaker error detection for short messages.

#![forbid(unsafe_code)]

const MOD_ADLER: u32 = 65521;

/// Computes the Adler-32 checksum of `data`.
#[must_use]
pub fn checksum(data: &[u8]) -> u32 {
    update(1, data)
}

/// Continues an incremental Adler-32 computation.
/// Pass the previous checksum as `adler`; pass `1` to start fresh.
#[must_use]
pub fn update(adler: u32, data: &[u8]) -> u32 {
    let mut a = adler & 0xFFFF;
    let mut b = (adler >> 16) & 0xFFFF;
    for &byte in data {
        a = (a + u32::from(byte)) % MOD_ADLER;
        b = (b + a) % MOD_ADLER;
    }
    (b << 16) | a
}

/// Computes the Adler-32 checksum of a UTF-8 string.
#[must_use]
pub fn checksum_string(s: &str) -> u32 {
    checksum(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_one() {
        // Adler-32 of empty input is 1 (A=1, B=0, combined = 0x00000001).
        assert_eq!(checksum(b""), 1);
    }

    #[test]
    fn wikipedia_known_value() {
        // From Wikipedia: Adler-32("Wikipedia") = 0x11E60398
        assert_eq!(checksum(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn incremental_matches_one_shot() {
        let data = b"hello, gossamer lang";
        let all_at_once = checksum(data);
        let incremental = update(update(1, &data[..5]), &data[5..]);
        assert_eq!(all_at_once, incremental);
    }

    #[test]
    fn checksum_string_matches_bytes() {
        let s = "hello";
        assert_eq!(checksum_string(s), checksum(s.as_bytes()));
    }
}
