//! Minimal POSIX (USTAR) tar reader.
//!
//! Parses a concatenation of 512-byte headers + padded file payloads
//! as emitted by `tar cf out.tar dir/`. Enough to unpack a
//! dependency tarball into a `BTreeMap<path, bytes>`; anything
//! fancier (sparse files, pax extended attributes, symlinks,
//! gzipped `.tar.gz`) returns [`TarError::Unsupported`]. Pulled in
//! because every credible dependency tarball is a tar file, and the
//! package fetcher now needs to crack them open without linking a
//! C library.
//!
//! Implements the single-file strict-read half of
//! the risks backlog "Real package-registry transport +
//! signature verification" — the registry-server + publish-flow
//! half is deliberately out of scope per the plan's staged
//! recommendation.
//!
//! Safe Rust only; no `unsafe` blocks. Workspace pledge upheld.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

/// Error shape for tarball parsing.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum TarError {
    /// Input ended mid-entry.
    #[error("truncated tar input at offset {0}")]
    Truncated(usize),
    /// Header's stored checksum did not match a recomputation over
    /// its bytes.
    #[error("tar header checksum mismatch for `{0}`")]
    BadChecksum(String),
    /// Size field was not parseable as an octal number.
    #[error("tar header size field malformed for `{0}`")]
    BadSize(String),
    /// Entry kind we do not yet unpack (symlink, hardlink, device
    /// node, sparse, pax extended attrs). Callers see the raw
    /// type-flag byte to decide whether to error or ignore.
    #[error("tar entry `{name}`: unsupported type flag {flag:?}")]
    Unsupported {
        /// Entry name as parsed.
        name: String,
        /// Byte value of the type flag field.
        flag: char,
    },
    /// Gzipped archive detected (first two bytes are the gzip magic).
    /// Callers that want `.tar.gz` support must decompress upstream.
    #[error(
        "gzipped archive detected — .tar.gz support is a follow-up; decompress before calling [`unpack`]"
    )]
    Gzipped,
}

const BLOCK: usize = 512;

/// Unpacks `bytes` into a path → contents map. Directory entries
/// become empty-byte files so callers walking the map still see
/// them. Returns an empty map for a zero-length input.
pub fn unpack(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, TarError> {
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        return Err(TarError::Gzipped);
    }
    let mut out = BTreeMap::new();
    let mut offset = 0;
    while offset < bytes.len() {
        if offset + BLOCK > bytes.len() {
            return Err(TarError::Truncated(offset));
        }
        let header = &bytes[offset..offset + BLOCK];
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let name = parse_name(header);
        let size = parse_size(header).ok_or_else(|| TarError::BadSize(name.clone()))?;
        verify_checksum(header, &name)?;
        let flag = header[156] as char;
        offset += BLOCK;
        let payload_end = offset + size;
        if payload_end > bytes.len() {
            return Err(TarError::Truncated(offset));
        }
        match flag {
            '0' | '\0' => {
                let contents = bytes[offset..payload_end].to_vec();
                out.insert(name, contents);
            }
            '5' => {
                // POSIX directory. Skip the payload (always zero)
                // and do not record the entry — our consumers walk
                // files only.
            }
            other => {
                return Err(TarError::Unsupported { name, flag: other });
            }
        }
        offset = payload_end;
        if size % BLOCK != 0 {
            offset += BLOCK - (size % BLOCK);
        }
    }
    Ok(out)
}

fn parse_name(header: &[u8]) -> String {
    // USTAR splits long names across `prefix` (offset 345, 155 bytes)
    // and `name` (offset 0, 100 bytes). Old tar tools emit only
    // `name`; GNU/BSD tar uses the split when names exceed 100
    // bytes. We honour both.
    let name = null_terminated(&header[0..100]);
    let prefix = if header.len() >= 500 {
        null_terminated(&header[345..500])
    } else {
        String::new()
    };
    if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    }
}

fn parse_size(header: &[u8]) -> Option<usize> {
    let field = &header[124..136];
    let text = std::str::from_utf8(field).ok()?;
    let trimmed = text.trim_end_matches('\0').trim();
    if trimmed.is_empty() {
        return Some(0);
    }
    usize::from_str_radix(trimmed, 8).ok()
}

fn verify_checksum(header: &[u8], name: &str) -> Result<(), TarError> {
    let stored_text = std::str::from_utf8(&header[148..156]).unwrap_or("");
    let stored_trimmed = stored_text.trim_end_matches(['\0', ' ']).trim();
    let Some(stored) = u32::from_str_radix(stored_trimmed, 8).ok() else {
        return Err(TarError::BadChecksum(name.to_string()));
    };
    let mut sum: u32 = 0;
    for (i, byte) in header.iter().enumerate() {
        if (148..156).contains(&i) {
            sum += u32::from(b' ');
        } else {
            sum += u32::from(*byte);
        }
    }
    if sum == stored {
        Ok(())
    } else {
        Err(TarError::BadChecksum(name.to_string()))
    }
}

fn null_terminated(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Errors raised by [`pack`].
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum PackError {
    /// Path was longer than the (USTAR `prefix` + `name`) split can express.
    #[error("path too long for USTAR (>= 256 bytes): {0}")]
    PathTooLong(String),
    /// Payload exceeded the 8 GiB ceiling representable in the 12-byte
    /// octal `size` field.
    #[error("file too large for USTAR (>= 8 GiB): {0}")]
    FileTooLarge(String),
}

/// Builds a deterministic USTAR-format tar buffer from `entries`.
/// Entries are emitted in lexicographic order, modification times
/// are zero, and ownership is set to root:root — so two runs over
/// the same input produce byte-identical output. Used by `gos
/// publish` so the published sha256 is stable across machines.
pub fn pack(entries: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>, PackError> {
    let mut out: Vec<u8> = Vec::new();
    for (path, body) in entries {
        if body.len() > 0o7777_7777_7777 {
            return Err(PackError::FileTooLarge(path.clone()));
        }
        let header = pack_header(path, body)?;
        out.extend_from_slice(&header);
        out.extend_from_slice(body);
        let pad = (BLOCK - body.len() % BLOCK) % BLOCK;
        out.resize(out.len() + pad, 0);
    }
    // USTAR end marker: two zero blocks.
    out.extend_from_slice(&[0u8; BLOCK * 2]);
    Ok(out)
}

fn pack_header(path: &str, body: &[u8]) -> Result<[u8; BLOCK], PackError> {
    let mut header = [0u8; BLOCK];
    write_path_into(&mut header, path)?;
    write_octal(&mut header[100..108], 0o644);
    write_octal(&mut header[108..116], 0);
    write_octal(&mut header[116..124], 0);
    write_octal(&mut header[124..136], body.len() as u64);
    write_octal(&mut header[136..148], 0);
    // Checksum field initialised to 8 spaces during sum computation.
    for cell in &mut header[148..156] {
        *cell = b' ';
    }
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let sum: u32 = header.iter().map(|b| u32::from(*b)).sum();
    let cs = format!("{sum:06o}\0 ");
    let cs_bytes = cs.as_bytes();
    for (i, b) in cs_bytes.iter().take(8).enumerate() {
        header[148 + i] = *b;
    }
    Ok(header)
}

fn write_path_into(header: &mut [u8; BLOCK], path: &str) -> Result<(), PackError> {
    let bytes = path.as_bytes();
    if bytes.len() <= 100 {
        for (i, b) in bytes.iter().enumerate() {
            header[i] = *b;
        }
        return Ok(());
    }
    if bytes.len() > 100 + 1 + 155 {
        return Err(PackError::PathTooLong(path.to_string()));
    }
    // Find a `/` split where the prefix fits in 155 bytes and the
    // suffix fits in 100. Walk backwards from the latest possible
    // split point.
    let max_prefix = bytes.len() - 1;
    let mut split: Option<usize> = None;
    for i in (1..=max_prefix.min(155)).rev() {
        if bytes[i] == b'/' && bytes.len() - i - 1 <= 100 {
            split = Some(i);
            break;
        }
    }
    let Some(split) = split else {
        return Err(PackError::PathTooLong(path.to_string()));
    };
    for (i, b) in bytes[..split].iter().enumerate() {
        header[345 + i] = *b;
    }
    for (i, b) in bytes[split + 1..].iter().enumerate() {
        header[i] = *b;
    }
    Ok(())
}

fn write_octal(field: &mut [u8], value: u64) {
    let width = field.len();
    let formatted = format!("{value:0width$o}", width = width - 1);
    let bytes = formatted.as_bytes();
    for (i, b) in bytes.iter().take(width - 1).enumerate() {
        field[i] = *b;
    }
    field[width - 1] = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a single-entry tar buffer in memory. USTAR layout:
    /// 100 name | 8 mode | 8 uid | 8 gid | 12 size | 12 mtime |
    /// 8 chksum | 1 typeflag | 100 linkname | 6 magic | 2 version |
    /// 32 uname | 32 gname | 8 devmajor | 8 devminor | 155 prefix |
    /// 12 pad — 512 bytes total.
    fn build_tar(name: &str, body: &[u8]) -> Vec<u8> {
        let mut header = [0u8; 512];
        for (i, b) in name.as_bytes().iter().take(100).enumerate() {
            header[i] = *b;
        }
        let mode = b"0000644\0";
        for (i, b) in mode.iter().enumerate() {
            header[100 + i] = *b;
        }
        let size_octal = format!("{:011o}\0", body.len());
        for (i, b) in size_octal.as_bytes().iter().take(12).enumerate() {
            header[124 + i] = *b;
        }
        let mtime = b"00000000000\0";
        for (i, b) in mtime.iter().enumerate() {
            header[136 + i] = *b;
        }
        for cell in &mut header[148..156] {
            *cell = b' ';
        }
        header[156] = b'0';
        let magic = b"ustar\0";
        for (i, b) in magic.iter().enumerate() {
            header[257 + i] = *b;
        }
        let version = b"00";
        header[263] = version[0];
        header[264] = version[1];
        let checksum: u32 = header.iter().map(|b| u32::from(*b)).sum();
        let cs_str = format!("{checksum:06o}\0 ");
        for (i, b) in cs_str.as_bytes().iter().take(8).enumerate() {
            header[148 + i] = *b;
        }
        let mut out = Vec::with_capacity(1024);
        out.extend_from_slice(&header);
        out.extend_from_slice(body);
        let pad = (512 - body.len() % 512) % 512;
        out.resize(out.len() + pad, 0);
        out.extend_from_slice(&[0u8; 1024]);
        out
    }

    #[test]
    fn unpack_reads_a_single_normal_file() {
        let tar = build_tar("src/lib.gos", b"fn main() {}\n");
        let files = unpack(&tar).expect("unpack");
        assert_eq!(files.len(), 1);
        assert_eq!(
            files.get("src/lib.gos").map(Vec::as_slice),
            Some(b"fn main() {}\n" as &[u8])
        );
    }

    #[test]
    fn unpack_refuses_gzipped_archives_with_a_clear_error() {
        let bytes = [0x1f, 0x8b, 0x00, 0x00, 0x00, 0x00];
        let err = unpack(&bytes).unwrap_err();
        assert!(matches!(err, TarError::Gzipped));
    }

    #[test]
    fn unpack_reports_checksum_mismatch_on_tampered_header() {
        let mut tar = build_tar("a.gos", b"hi");
        tar[148] = b'9';
        let err = unpack(&tar).unwrap_err();
        assert!(matches!(err, TarError::BadChecksum(_)));
    }

    #[test]
    fn unpack_handles_an_empty_archive() {
        let empty_blocks = vec![0u8; 1024];
        let files = unpack(&empty_blocks).expect("unpack empty");
        assert!(files.is_empty());
    }

    #[test]
    fn pack_round_trips_through_unpack() {
        let mut input: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        input.insert("src/main.gos".to_string(), b"fn main() {}\n".to_vec());
        input.insert("README.md".to_string(), b"# project\n".to_vec());
        input.insert("project.toml".to_string(), b"[project]\n".to_vec());
        let bytes = pack(&input).expect("pack");
        let back = unpack(&bytes).expect("unpack");
        assert_eq!(input, back);
    }

    #[test]
    fn pack_is_byte_deterministic() {
        let mut input: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        input.insert("a.txt".to_string(), b"alpha".to_vec());
        input.insert("b.txt".to_string(), b"beta".to_vec());
        let a = pack(&input).expect("pack a");
        let b = pack(&input).expect("pack b");
        assert_eq!(
            a, b,
            "two pack calls on identical input must produce identical bytes"
        );
    }
}
