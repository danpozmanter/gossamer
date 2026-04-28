//! Runtime support for `std::fs` — filesystem walking + mutation
//! helpers on top of `std::fs`.

#![forbid(unsafe_code)]

use std::fs::{self as stdfs, Metadata};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Directory entry surfaced by [`read_dir`].
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// Full path to the entry.
    pub path: PathBuf,
    /// File name within the parent directory.
    pub name: String,
    /// `true` when the entry is a directory.
    pub is_dir: bool,
    /// `true` when the entry is a regular file.
    pub is_file: bool,
    /// `true` when the entry is a symlink.
    pub is_symlink: bool,
}

/// Lists the direct children of `path`. Does not recurse.
pub fn read_dir(path: impl AsRef<Path>) -> io::Result<Vec<DirEntry>> {
    let mut out = Vec::new();
    for raw in stdfs::read_dir(path)? {
        let raw = raw?;
        let ty = raw.file_type()?;
        out.push(DirEntry {
            path: raw.path(),
            name: raw.file_name().to_string_lossy().into_owned(),
            is_dir: ty.is_dir(),
            is_file: ty.is_file(),
            is_symlink: ty.is_symlink(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Recursively walks `root`, invoking `visit` for every entry.
/// Traversal is depth-first; directories are visited before their
/// children. Returns as soon as `visit` returns an `Err`.
pub fn walk_dir<F>(root: impl AsRef<Path>, mut visit: F) -> io::Result<()>
where
    F: FnMut(&DirEntry) -> io::Result<()>,
{
    let mut stack: Vec<PathBuf> = vec![root.as_ref().to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in read_dir(&dir)? {
            visit(&entry)?;
            if entry.is_dir {
                stack.push(entry.path.clone());
            }
        }
    }
    Ok(())
}

/// Creates `path` and every missing ancestor, mirroring `mkdir -p`.
pub fn create_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    stdfs::create_dir_all(path)
}

/// Removes `path` and everything underneath, if `path` is a
/// directory; or deletes a single file otherwise.
pub fn remove_all(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        stdfs::remove_dir_all(path)
    } else {
        stdfs::remove_file(path)
    }
}

/// Copies `src` to `dst`, creating the destination's parent dirs if
/// needed. Returns the number of bytes copied.
pub fn copy(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> io::Result<u64> {
    let dst = dst.as_ref();
    if let Some(parent) = dst.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            stdfs::create_dir_all(parent)?;
        }
    }
    stdfs::copy(src, dst)
}

/// Renames `src` to `dst`.
pub fn rename(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> io::Result<()> {
    stdfs::rename(src, dst)
}

/// Returns the [`Metadata`] for `path`.
pub fn metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
    stdfs::metadata(path)
}

/// Reads the entire contents of `path` into a string. Routes the
/// blocking read through the goroutine-aware blocking thread pool
/// so the calling worker P slot is freed for other goroutines.
pub fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    let path = path.as_ref().to_path_buf();
    crate::blocking_pool::run(move || {
        let mut file = stdfs::File::open(&path)?;
        let mut out = String::new();
        file.read_to_string(&mut out)?;
        Ok(out)
    })
}

/// Writes `contents` to `path`, truncating any existing file and
/// creating parent directories if needed. Same blocking-pool dispatch
/// as [`read_to_string`].
pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let path = path.as_ref().to_path_buf();
    let bytes = contents.as_ref().to_vec();
    crate::blocking_pool::run(move || {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                stdfs::create_dir_all(parent)?;
            }
        }
        let mut file = stdfs::File::create(&path)?;
        file.write_all(&bytes)?;
        Ok(())
    })
}

/// Returns `true` iff `path` exists.
pub fn exists(path: impl AsRef<Path>) -> bool {
    path.as_ref().exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("gos-fs-{tag}-{}", std::process::id()));
        let _ = stdfs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = scratch("wr");
        let path = dir.join("nested/file.txt");
        write(&path, "hello").unwrap();
        let text = read_to_string(&path).unwrap();
        assert_eq!(text, "hello");
        let _ = remove_all(&dir);
    }

    #[test]
    fn walk_dir_visits_every_descendant() {
        let dir = scratch("walk");
        write(dir.join("a/one.txt"), "1").unwrap();
        write(dir.join("a/two.txt"), "2").unwrap();
        write(dir.join("b/three.txt"), "3").unwrap();
        let mut names: Vec<String> = Vec::new();
        walk_dir(&dir, |entry| {
            if entry.is_file {
                names.push(entry.name.clone());
            }
            Ok(())
        })
        .unwrap();
        names.sort();
        assert_eq!(names, vec!["one.txt", "three.txt", "two.txt"]);
        let _ = remove_all(&dir);
    }

    #[test]
    fn copy_creates_missing_parents() {
        let dir = scratch("copy");
        write(dir.join("src.txt"), "hi").unwrap();
        copy(dir.join("src.txt"), dir.join("nested/out.txt")).unwrap();
        assert!(exists(dir.join("nested/out.txt")));
        let _ = remove_all(&dir);
    }

    #[test]
    fn remove_all_deletes_tree() {
        let dir = scratch("rm");
        write(dir.join("a/b/c.txt"), "x").unwrap();
        remove_all(&dir).unwrap();
        assert!(!exists(&dir));
    }
}
