// Runtime support for `std::hash::crc32` - CRC-32 checksums.
//
// Implements the IEEE 802.3 (Ethernet) polynomial, which is what Go's
// hash/crc32 calls IEEE and uses for gzip/zlib checksums.

#![forbid(unsafe_code)]

const fn crc32_tables() -> [[u32; 256]; 8] {
    let mut tables = [[0u32; 256]; 8];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            crc = if crc & 1 != 0 {
                0xEDB8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
            j += 1;
        }
        tables[0][i] = crc;
        i += 1;
    }
    let mut i = 0usize;
    while i < 256 {
        let mut crc = tables[0][i];
        let mut k = 1;
        while k < 8 {
            crc = tables[0][(crc & 0xFF) as usize] ^ (crc >> 8);
            tables[k][i] = crc;
            k += 1;
        }
        i += 1;
    }
    tables
}

// Slicing-by-eight: one table lookup per byte with eight independent lookups
// per word, so the loop is bound by table loads rather than by the dependency
// chain of the byte-at-a-time form.
static CRC32_TABLES: [[u32; 256]; 8] = crc32_tables();

fn crc32_slice_by_eight(mut state: u32, data: &[u8]) -> u32 {
    let mut chunks = data.chunks_exact(8);
    for chunk in &mut chunks {
        let lo = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) ^ state;
        let hi = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
        state = CRC32_TABLES[7][(lo & 0xFF) as usize]
            ^ CRC32_TABLES[6][((lo >> 8) & 0xFF) as usize]
            ^ CRC32_TABLES[5][((lo >> 16) & 0xFF) as usize]
            ^ CRC32_TABLES[4][(lo >> 24) as usize]
            ^ CRC32_TABLES[3][(hi & 0xFF) as usize]
            ^ CRC32_TABLES[2][((hi >> 8) & 0xFF) as usize]
            ^ CRC32_TABLES[1][((hi >> 16) & 0xFF) as usize]
            ^ CRC32_TABLES[0][(hi >> 24) as usize];
    }
    for &byte in chunks.remainder() {
        state = CRC32_TABLES[0][((state ^ u32::from(byte)) & 0xFF) as usize] ^ (state >> 8);
    }
    state
}

/// Computes the IEEE CRC-32 checksum of `data`.
#[must_use]
pub fn checksum(data: &[u8]) -> u32 {
    update(0, data)
}

/// Continues an incremental CRC-32 computation.
/// Pass the previous checksum as `crc`; pass `0` to start fresh.
#[must_use]
pub fn update(crc: u32, data: &[u8]) -> u32 {
    !crc32_slice_by_eight(!crc, data)
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
    fn check_value_and_unaligned_splits_match() {
        // The CRC-32 check value: the checksum of the ASCII digits 1-9.
        assert_eq!(checksum(b"123456789"), 0xCBF4_3926);
        let data: Vec<u8> = (0..251u8).map(|b| b.wrapping_mul(37)).collect();
        let whole = checksum(&data);
        for split in [1usize, 3, 7, 8, 9, 15, 16, 17, 100, 250] {
            assert_eq!(update(update(0, &data[..split]), &data[split..]), whole);
        }
    }

    #[test]
    fn checksum_string_matches_bytes() {
        let s = "hello";
        assert_eq!(checksum_string(s), checksum(s.as_bytes()));
    }
}
