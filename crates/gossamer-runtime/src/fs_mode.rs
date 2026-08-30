//! Permission bits on a path, in one place for every tier.
//!
//! `fs::set_permissions`, `fs::permissions`, and the create-with-mode
//! calls answer the same thing under `gos run` as they do in a
//! compiled binary because both reach this module: the bytecode VM's
//! builtins call it directly and the `gos_rt_fs_*` shims call it
//! behind the C ABI.
//!
//! A mode is the chmod(2) encoding on Unix. Windows has no permission
//! bits at all - the only permission an NTFS path exposes is whether
//! it is read-only - so there the owner write bit decides that one
//! attribute and every other bit is ignored, because the platform has
//! nothing for them to mean.

use std::io;
use std::path::Path;

/// Gives `path` exactly the permissions `mode` names.
#[cfg(unix)]
pub fn apply(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// Sets or clears the read-only attribute from the owner write bit.
#[cfg(windows)]
pub fn apply(path: &Path, mode: u32) -> io::Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_readonly(mode & 0o200 == 0);
    std::fs::set_permissions(path, permissions)
}

/// Stub for a target with neither Unix modes nor the Windows
/// read-only attribute.
#[cfg(not(any(unix, windows)))]
pub fn apply(_path: &Path, _mode: u32) -> io::Result<()> {
    Err(unsupported())
}

/// The permission bits of `path`, in the chmod(2) encoding, including
/// the setuid, setgid, and sticky bits.
#[cfg(unix)]
pub fn read(path: &Path) -> io::Result<u32> {
    use std::os::unix::fs::PermissionsExt;
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o7777)
}

/// The read-only attribute widened into the bits an equivalent Unix
/// path would carry, so one value tests and re-applies on both.
#[cfg(windows)]
pub fn read(path: &Path) -> io::Result<u32> {
    let metadata = std::fs::metadata(path)?;
    let base = if metadata.is_dir() { 0o777 } else { 0o666 };
    Ok(if metadata.permissions().readonly() {
        base & !0o222
    } else {
        base
    })
}

/// Stub for a target with neither Unix modes nor the Windows
/// read-only attribute.
#[cfg(not(any(unix, windows)))]
pub fn read(_path: &Path) -> io::Result<u32> {
    Err(unsupported())
}

#[cfg(not(any(unix, windows)))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "permission bits are unsupported on this target",
    )
}

/// Creates the directory `path` and gives it exactly `mode`.
///
/// The mode is stated after the directory exists, so the process umask
/// cannot mask a bit out of it: `mkdir -m 0777` is the same two steps,
/// and a directory a tool requires to be world-writable is
/// world-writable however the umask is set.
pub fn create_dir(path: &Path, mode: u32) -> io::Result<()> {
    std::fs::create_dir(path)?;
    apply(path, mode)
}

/// Creates `path` and every missing parent, giving `mode` to each
/// directory this call creates and leaving one that already existed as
/// it is.
pub fn create_dir_all(path: &Path, mode: u32) -> io::Result<()> {
    if path.as_os_str().is_empty() || path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        create_dir_all(parent, mode)?;
    }
    match std::fs::create_dir(path) {
        Ok(()) => apply(path, mode),
        // Whoever won the race left a directory in place, which is
        // what the caller asked for; its mode belongs to them.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(error) => Err(error),
    }
}

/// Writes `bytes` to `path` and leaves the file at exactly `mode`.
///
/// Created with the mode first, so the file is never more permissive
/// than asked for, and stated again afterwards, so the umask cannot
/// leave it less permissive than asked for.
pub fn write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    drop(file);
    apply(path, mode)
}

/// The permission bits a Gossamer `i64` mode names.
///
/// A mode is written as an octal literal, so anything outside the
/// twelve bits chmod(2) defines is not a permission and is dropped
/// rather than reinterpreted.
#[must_use]
pub fn bits(mode: i64) -> u32 {
    u32::try_from(mode).unwrap_or(0) & 0o7777
}

#[cfg(test)]
mod fs_mode_tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("gos-fs-mode-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch");
        dir
    }

    #[test]
    fn a_mode_outside_the_permission_bits_is_dropped() {
        assert_eq!(bits(0o755), 0o755);
        assert_eq!(bits(0o7777), 0o7777);
        assert_eq!(bits(0o10_755), 0o755);
        assert_eq!(bits(-1), 0);
    }

    #[test]
    #[cfg_attr(miri, ignore)] // sets real permission bits; Miri cannot call chmod(2)
    fn a_directory_is_created_with_the_mode_it_asked_for() {
        let dir = scratch("create");
        let made = dir.join("shared");
        create_dir(&made, 0o777).unwrap();
        assert!(made.is_dir());
        assert_eq!(read(&made).unwrap(), 0o777);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg_attr(miri, ignore)] // sets real permission bits; Miri cannot call chmod(2)
    fn every_directory_a_recursive_create_makes_gets_the_mode() {
        let dir = scratch("create-all");
        let leaf = dir.join("a").join("b");
        create_dir_all(&leaf, 0o707).unwrap();
        #[cfg(unix)]
        for made in [dir.join("a"), leaf.clone()] {
            assert_eq!(read(&made).unwrap(), 0o707, "{}", made.display());
        }
        // A second call over an existing tree is not an error and
        // re-modes nothing.
        create_dir_all(&leaf, 0o700).unwrap();
        #[cfg(unix)]
        assert_eq!(read(&leaf).unwrap(), 0o707);
        #[cfg(unix)]
        apply(&dir.join("a"), 0o755).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg_attr(miri, ignore)] // sets real permission bits; Miri cannot call chmod(2)
    fn a_write_leaves_the_file_at_the_mode_whatever_it_was_before() {
        let dir = scratch("write");
        let path = dir.join("f.txt");
        write(&path, b"one", 0o600).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
        #[cfg(unix)]
        assert_eq!(read(&path).unwrap(), 0o600);
        write(&path, b"two", 0o644).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        #[cfg(unix)]
        assert_eq!(read(&path).unwrap(), 0o644);
        // The write bit is the one every platform carries.
        assert_ne!(read(&path).unwrap() & 0o200, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_path_has_no_permissions_to_read() {
        let dir = scratch("missing");
        let error = read(&dir.join("nope")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
