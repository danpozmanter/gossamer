// Runtime support for `std::hash::crc32` — CRC-32 checksums.
//
// Implements the IEEE 802.3 (Ethernet) polynomial, which is what Go's
// hash/crc32 calls IEEE and uses for gzip/zlib checksums.

#![forbid(unsafe_code)]

// Precomputed IEEE polynomial table.
const fn make_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = 0xEDB8_8320 ^ (crc >> 1);
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRC32_TABLE: [u32; 256] = make_table();

/// Computes the IEEE CRC-32 checksum of `data`.
#[must_use]
pub fn checksum(data: &[u8]) -> u32 {
    update(0, data)
}

/// Continues an incremental CRC-32 computation.
/// Pass the previous checksum as `crc`; pass `0` to start fresh.
#[must_use]
pub fn update(crc: u32, data: &[u8]) -> u32 {
    let mut state = !crc;
    for &byte in data {
        let idx = ((state ^ u32::from(byte)) & 0xFF) as usize;
        state = CRC32_TABLE[idx] ^ (state >> 8);
    }
    !state
}

/// Computes the IEEE CRC-32 checksum of a UTF-8 string.
#[must_use]
pub fn checksum_string(s: &str) -> u32 {
    checksum(s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero() {
        assert_eq!(checksum(b""), 0);
    }

    #[test]
    fn hello_world_known_value() {
        // echo -n "hello world" | crc32 /dev/stdin → 0x0D4A1185
        assert_eq!(checksum(b"hello world"), 0x0D4A_1185);
    }

    #[test]
    fn incremental_matches_one_shot() {
        let data = b"hello, gossamer lang";
        let all_at_once = checksum(data);
        let incremental = update(update(0, &data[..5]), &data[5..]);
        assert_eq!(all_at_once, incremental);
    }

    #[test]
    fn checksum_string_matches_bytes() {
        let s = "hello";
        assert_eq!(checksum_string(s), checksum(s.as_bytes()));
    }
}
