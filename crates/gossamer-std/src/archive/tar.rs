// Runtime support for `std::archive::tar` - tar archive reading and writing.
//
// Wraps the `tar` crate. The read API extracts regular files into memory; the
// write API builds an in-memory tar archive from name/content pairs.

#![forbid(unsafe_code)]

use std::io::Cursor;

use crate::io::IoError;

/// A single file entry extracted from a tar archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TarEntry {
    /// Path inside the archive.
    pub name: String,
    /// File content (empty for non-regular entries).
    pub data: Vec<u8>,
    /// `true` for directory entries.
    pub is_dir: bool,
}

/// Reads all regular-file and directory entries from a tar archive in `data`.
pub fn read(data: &[u8]) -> Result<Vec<TarEntry>, IoError> {
    let cursor = Cursor::new(data);
    let mut archive = tar::Archive::new(cursor);
    let mut entries = Vec::new();
    for entry in archive
        .entries()
        .map_err(|e| IoError::Other(format!("tar entries: {e}")))?
    {
        let mut entry = entry.map_err(|e| IoError::Other(format!("tar entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| IoError::Other(format!("tar entry path: {e}")))?;
        let name = path.to_string_lossy().into_owned();
        let kind = entry.header().entry_type();
        let is_dir = kind.is_dir();
        let mut buf = Vec::new();
        if kind.is_file() {
            std::io::Read::read_to_end(&mut entry, &mut buf)
                .map_err(|e| IoError::Other(format!("tar read {name}: {e}")))?;
        }
        entries.push(TarEntry {
            name,
            data: buf,
            is_dir,
        });
    }
    Ok(entries)
}

/// Builds an in-memory (ustar) tar archive from `files` - `(name, data)` pairs.
pub fn write(files: &[(&str, &[u8])]) -> Result<Vec<u8>, IoError> {
    let buf = Vec::new();
    let mut builder = tar::Builder::new(buf);
    for &(name, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, name, data)
            .map_err(|e| IoError::Other(format!("tar append {name}: {e}")))?;
    }
    let out = builder
        .into_inner()
        .map_err(|e| IoError::Other(format!("tar finish: {e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_single_file() {
        let content = b"hello from tar";
        let tar_bytes = write(&[("hello.txt", content)]).unwrap();
        let entries = read(&tar_bytes).unwrap();
        assert_eq!(entries.len(), 1);
        // tar appends a trailing slash for dirs; files keep the path
        assert!(entries[0].name.contains("hello.txt"));
        assert_eq!(entries[0].data, content);
        assert!(!entries[0].is_dir);
    }

    #[test]
    fn roundtrip_multiple_files() {
        let tar_bytes = write(&[("a.txt", b"aaa"), ("b.txt", b"bbb")]).unwrap();
        let entries = read(&tar_bytes).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn empty_archive() {
        let tar_bytes = write(&[]).unwrap();
        let entries = read(&tar_bytes).unwrap();
        assert!(entries.is_empty());
    }
}
