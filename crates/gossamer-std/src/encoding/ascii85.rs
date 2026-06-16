// Runtime support for `std::encoding::ascii85` - Adobe ASCII85 / btoa.
//
// Encodes arbitrary binary data as printable ASCII using characters in
// the range `!` (0x21) through `u` (0x75). The special case `z`
// represents a group of four zero bytes. Output is wrapped in `<~`
// and `~>` delimiters by default and decoded stripping them. Whitespace
// in the encoded stream is silently ignored during decode.

#![forbid(unsafe_code)]

use crate::errors::Error;

/// Encodes `data` as an ASCII85 string, wrapped in `<~` … `~>`.
#[must_use]
pub fn encode(data: &[u8]) -> String {
    let mut out = String::from("<~");
    let mut i = 0;
    while i < data.len() {
        let remaining = data.len() - i;
        let chunk = &data[i..i + remaining.min(4)];
        let mut group = [0u8; 4];
        group[..chunk.len()].copy_from_slice(chunk);
        let val = u32::from_be_bytes(group);
        if chunk.len() == 4 && val == 0 {
            out.push('z');
        } else {
            let mut digits = [0u8; 5];
            let mut v = val;
            for d in digits.iter_mut().rev() {
                *d = (v % 85) as u8 + b'!';
                v /= 85;
            }
            for &d in &digits[..=chunk.len()] {
                out.push(char::from(d));
            }
        }
        i += chunk.len();
    }
    out.push_str("~>");
    out
}

/// Decodes an ASCII85 string. Accepts optional `<~` … `~>` delimiters and
/// silently skips whitespace.
pub fn decode(s: &str) -> Result<Vec<u8>, Error> {
    let s = s.trim();
    let s = s.strip_prefix("<~").unwrap_or(s);
    let s = s.strip_suffix("~>").unwrap_or(s);
    let mut out = Vec::new();
    let mut group = [0u8; 5];
    let mut count = 0usize;
    for ch in s.chars() {
        if ch.is_ascii_whitespace() {
            continue;
        }
        if ch == 'z' {
            if count != 0 {
                return Err(Error::new("ascii85: z inside group"));
            }
            out.extend_from_slice(&[0u8; 4]);
            continue;
        }
        if !(b'!'..=b'u').contains(&(ch as u8)) {
            return Err(Error::new(format!("ascii85: invalid character '{ch}'")));
        }
        group[count] = ch as u8 - b'!';
        count += 1;
        if count == 5 {
            let val: u32 = u32::from(group[0]) * 52200625
                + u32::from(group[1]) * 614125
                + u32::from(group[2]) * 7225
                + u32::from(group[3]) * 85
                + u32::from(group[4]);
            out.extend_from_slice(&val.to_be_bytes());
            count = 0;
        }
    }
    if count > 0 {
        if count == 1 {
            return Err(Error::new("ascii85: trailing single digit"));
        }
        let padding = 5 - count;
        for slot in group.iter_mut().skip(count) {
            *slot = b'u' - b'!';
        }
        let val: u32 = u32::from(group[0]) * 52200625
            + u32::from(group[1]) * 614125
            + u32::from(group[2]) * 7225
            + u32::from(group[3]) * 85
            + u32::from(group[4]);
        let bytes = val.to_be_bytes();
        out.extend_from_slice(&bytes[..4 - padding]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        for s in ["", "Man", "hello, world!", "gossamer lang"] {
            let enc = encode(s.as_bytes());
            let dec = decode(&enc).unwrap();
            assert_eq!(dec, s.as_bytes(), "roundtrip failed for {s:?}");
        }
    }

    #[test]
    fn zero_group_is_z() {
        let enc = encode(&[0u8; 4]);
        assert!(enc.contains('z'));
    }

    #[test]
    fn invalid_char_returns_err() {
        assert!(decode("<~@~>").is_err());
    }
}
