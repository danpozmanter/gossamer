//! Runtime support for `std::fs` - filesystem walking + mutation
//! helpers on top of `std::fs`.

use std::fs::{self as stdfs, File, Metadata};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use notify::{
    Event as NotifyEvent, EventKind as NotifyEventKind, RecommendedWatcher, RecursiveMode,
    Watcher as NotifyWatcherTrait,
};
use parking_lot::Mutex;

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

/// Reads the entire contents of `path` into a byte vector.
pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let path = path.as_ref().to_path_buf();
    crate::blocking_pool::run(move || stdfs::read(&path))
}

/// `true` iff `path` exists and is a regular file.
#[must_use]
pub fn is_file(path: impl AsRef<Path>) -> bool {
    path.as_ref().is_file()
}

/// `true` iff `path` exists and is a directory.
#[must_use]
pub fn is_dir(path: impl AsRef<Path>) -> bool {
    path.as_ref().is_dir()
}

/// `true` iff `path` exists and is a symbolic link.
#[must_use]
pub fn is_symlink(path: impl AsRef<Path>) -> bool {
    stdfs::symlink_metadata(path.as_ref()).is_ok_and(|m| m.file_type().is_symlink())
}

/// File size in bytes, or 0 if `path` cannot be stat'd.
#[must_use]
pub fn file_size(path: impl AsRef<Path>) -> u64 {
    stdfs::metadata(path).map_or(0, |m| m.len())
}

/// Resolves `path` to an absolute, symlink-free form.
pub fn canonicalize(path: impl AsRef<Path>) -> io::Result<String> {
    stdfs::canonicalize(path).map(|p| p.to_string_lossy().into_owned())
}

/// Creates a single directory at `path`. Fails if a parent is
/// missing - use [`create_dir_all`] for the recursive form.
pub fn create_dir(path: impl AsRef<Path>) -> io::Result<()> {
    stdfs::create_dir(path)
}

/// Removes a single file.
pub fn remove_file(path: impl AsRef<Path>) -> io::Result<()> {
    stdfs::remove_file(path)
}

/// Removes an empty directory.
pub fn remove_dir(path: impl AsRef<Path>) -> io::Result<()> {
    stdfs::remove_dir(path)
}

/// Recursively removes a directory and its contents.
pub fn remove_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    stdfs::remove_dir_all(path)
}

/// Returns paths matching the glob `pattern`. Supports `*`, `?`,
/// `[abc]`, and `**` (recursive directory match). The pattern is
/// rooted at the current working directory unless it begins
/// with `/`.
pub fn glob(pattern: &str) -> io::Result<Vec<String>> {
    crate::path::glob(pattern)
}

/// Resolves all symlinks along `path` and returns the canonical
/// absolute path. Same shape as [`canonicalize`] but mirrors Go's
/// `filepath.EvalSymlinks` name.
pub fn eval_symlinks(path: impl AsRef<Path>) -> io::Result<String> {
    crate::path::eval_symlinks(path)
}

/// Creates a hard link `dst` pointing at the same inode as `src`.
/// Wraps [`std::fs::hard_link`].
pub fn hard_link(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> io::Result<()> {
    stdfs::hard_link(src, dst)
}

/// Sets the Unix permission bits on `path` from `mode`. The bits
/// match the chmod(2) value (e.g. `0o755`). Returns
/// `ErrorKind::Unsupported` on non-Unix platforms.
#[cfg(unix)]
pub fn set_permissions_mode(path: impl AsRef<Path>, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    stdfs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// Non-Unix stub for [`set_permissions_mode`]. Returns
/// `ErrorKind::Unsupported`.
#[cfg(not(unix))]
pub fn set_permissions_mode(_path: impl AsRef<Path>, _mode: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "set_permissions_mode is only supported on Unix targets",
    ))
}

/// Changes the owner and / or group of `path`. Pass `-1` for `uid`
/// or `gid` to leave that side unchanged. Returns
/// `ErrorKind::Unsupported` on non-Unix platforms.
#[cfg(unix)]
pub fn chown(path: impl AsRef<Path>, uid: i64, gid: i64) -> io::Result<()> {
    use nix::unistd::{Gid, Uid};
    let uid_arg = if uid < 0 {
        None
    } else {
        Some(Uid::from_raw(uid as u32))
    };
    let gid_arg = if gid < 0 {
        None
    } else {
        Some(Gid::from_raw(gid as u32))
    };
    nix::unistd::chown(path.as_ref(), uid_arg, gid_arg).map_err(io::Error::from)
}

/// Non-Unix stub for [`chown`]. Returns `ErrorKind::Unsupported`.
#[cfg(not(unix))]
pub fn chown(_path: impl AsRef<Path>, _uid: i64, _gid: i64) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "chown is only supported on Unix targets",
    ))
}

/// Writes `bytes` to `path` atomically: the bytes are first written
/// to a sibling temp file, fsync'd, then renamed into place. On a
/// crash mid-write the caller observes either the previous file
/// contents or the new ones - never a partial file.
pub fn write_atomic(path: impl AsRef<Path>, bytes: &[u8]) -> io::Result<()> {
    let path = path.as_ref();
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "write_atomic: path has no file name",
            )
        })?
        .to_string_lossy()
        .into_owned();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = parent.join(format!("{file_name}.tmp.{}.{nanos}", std::process::id()));

    if !parent.as_os_str().is_empty() && !parent.exists() {
        stdfs::create_dir_all(parent)?;
    }

    let result = (|| -> io::Result<()> {
        let mut file = stdfs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        stdfs::rename(&tmp, path)
    })();

    if result.is_err() {
        let _ = stdfs::remove_file(&tmp);
    }
    result
}

/// Kind of filesystem change reported by [`Watcher`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// A new file or directory appeared at the path.
    Created,
    /// An existing path's contents or metadata changed.
    Modified,
    /// A file or directory was deleted.
    Removed,
}

/// A single filesystem change observed by [`Watcher`].
#[derive(Debug, Clone)]
pub struct Event {
    /// Path that changed.
    pub path: String,
    /// Nature of the change.
    pub kind: EventKind,
}

fn translate_event_kind(kind: NotifyEventKind) -> Option<EventKind> {
    match kind {
        NotifyEventKind::Create(_) => Some(EventKind::Created),
        NotifyEventKind::Modify(_) => Some(EventKind::Modified),
        NotifyEventKind::Remove(_) => Some(EventKind::Removed),
        _ => None,
    }
}

/// Recursive filesystem watcher. Holds an underlying
/// [`notify`] recommended watcher and a channel of translated
/// [`Event`] values. Dropping the watcher stops further
/// notifications.
pub struct Watcher {
    inner: Mutex<RecommendedWatcher>,
    rx: Mutex<Option<Receiver<Event>>>,
    #[cfg_attr(not(test), allow(dead_code))]
    tx: Sender<Event>,
}

impl Watcher {
    /// Constructs a new watcher wired to the platform's native
    /// notification backend. The receiver returned by
    /// [`Watcher::events`] yields one event per observed change.
    pub fn new() -> io::Result<Self> {
        let (tx, rx) = channel::<Event>();
        let event_tx = tx.clone();
        let inner = notify::recommended_watcher(move |res: Result<NotifyEvent, notify::Error>| {
            if let Ok(ev) = res {
                if let Some(kind) = translate_event_kind(ev.kind) {
                    for path in ev.paths {
                        let _ = event_tx.send(Event {
                            path: path.to_string_lossy().into_owned(),
                            kind,
                        });
                    }
                }
            }
        })
        .map_err(notify_to_io)?;
        Ok(Self {
            inner: Mutex::new(inner),
            rx: Mutex::new(Some(rx)),
            tx,
        })
    }

    /// Starts watching `path` recursively. The path must exist.
    pub fn add(&self, path: &str) -> io::Result<()> {
        let mut guard = self.inner.lock();
        guard
            .watch(Path::new(path), RecursiveMode::Recursive)
            .map_err(notify_to_io)
    }

    /// Takes ownership of the event receiver. Subsequent calls return
    /// `None` because a `Receiver` cannot be cloned.
    pub fn events(&self) -> Option<Receiver<Event>> {
        self.rx.lock().take()
    }

    /// Internal hook used by tests to inject a synthetic event.
    #[cfg(test)]
    fn inject_for_test(&self, event: Event) {
        let _ = self.tx.send(event);
    }
}

fn notify_to_io(err: notify::Error) -> io::Error {
    match err.kind {
        notify::ErrorKind::Io(e) => e,
        _ => io::Error::other(err.to_string()),
    }
}

/// Read-only memory map of a file. The map is valid for as long
/// as the `Mmap` is alive; dropping it unmaps the region via
/// the platform's `munmap` / `UnmapViewOfFile`.
#[derive(Debug)]
pub struct Mmap {
    inner: memmap2::Mmap,
}

impl Mmap {
    /// Returns the mapped bytes as a borrowed slice. The slice is
    /// valid for the lifetime of the `Mmap`.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.inner
    }

    /// Length of the mapped region in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns whether the map covers zero bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// Memory-maps `path` for read-only access. Returns
/// `ErrorKind::InvalidInput` if the file is empty (most platforms
/// reject zero-length mappings).
pub fn mmap_read(path: &str) -> io::Result<Mmap> {
    let file = stdfs::File::open(path)?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "mmap_read: cannot map a zero-length file",
        ));
    }
    // SAFETY: caller guarantees the file's bytes are stable for
    // the duration of the returned `Mmap`. The wrapper is read-only
    // and never aliased mutably.
    #[allow(unsafe_code)]
    let inner = unsafe { memmap2::Mmap::map(&file)? };
    Ok(Mmap { inner })
}

/// Acquires an exclusive (writer) advisory lock on `file`. Blocks
/// until any conflicting lock is released. Locks are advisory -
/// only cooperating processes that also call the lock helpers
/// see them.
pub fn lock_exclusive(file: &File) -> io::Result<()> {
    fs2::FileExt::lock_exclusive(file)
}

/// Acquires a shared (reader) advisory lock on `file`. Multiple
/// shared locks may coexist; an exclusive lock blocks them.
pub fn lock_shared(file: &File) -> io::Result<()> {
    fs2::FileExt::lock_shared(file)
}

/// Non-blocking variant of [`lock_exclusive`]. Returns
/// `ErrorKind::WouldBlock` immediately when a conflicting lock
/// is held.
pub fn try_lock_exclusive(file: &File) -> io::Result<()> {
    fs2::FileExt::try_lock_exclusive(file).map_err(normalize_try_lock_err)
}

/// Non-blocking variant of [`lock_shared`]. Returns
/// `ErrorKind::WouldBlock` immediately when a conflicting lock
/// is held.
pub fn try_lock_shared(file: &File) -> io::Result<()> {
    fs2::FileExt::try_lock_shared(file).map_err(normalize_try_lock_err)
}

/// Normalizes platform-specific lock-contention errors so the
/// documented `try_lock_*` contract holds on every platform.
///
/// POSIX `flock` returns `EAGAIN`/`EWOULDBLOCK` (Rust maps both
/// to `ErrorKind::WouldBlock`). Windows `LockFileEx` with
/// `LOCKFILE_FAIL_IMMEDIATELY` returns `ERROR_LOCK_VIOLATION` (33),
/// which Rust's `decode_error_kind` table does not list - it
/// surfaces as the private `ErrorKind::Uncategorized`, breaking
/// callers that match on `WouldBlock`. Re-stamp the kind here so
/// the contract is the same shape everywhere.
fn normalize_try_lock_err(e: io::Error) -> io::Error {
    #[cfg(windows)]
    if e.raw_os_error() == Some(33) {
        return io::Error::new(io::ErrorKind::WouldBlock, e);
    }
    e
}

/// Releases any advisory lock previously taken on `file`. Idempotent
/// - releasing an already-unlocked handle is not an error on POSIX.
pub fn unlock(file: &File) -> io::Result<()> {
    fs2::FileExt::unlock(file)
}

/// RAII wrapper around a freshly-created temporary directory. The
/// directory and every entry inside it are removed when the handle
/// drops - even if the holder panicked. Mirrors Python's
/// `tempfile.TemporaryDirectory`.
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Creates a new unique directory under the system temp root.
    /// The name uses the process id + a monotonic counter; never
    /// reuses a name within the lifetime of a process.
    pub fn new() -> io::Result<Self> {
        Self::with_prefix("tmp")
    }

    /// Like [`Self::new`] but the directory name carries the
    /// caller-supplied `prefix` for easier identification in
    /// long-lived working directories.
    pub fn with_prefix(prefix: &str) -> io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let mut path = std::env::temp_dir();
        path.push(format!("gossamer-{prefix}-{pid}-{nanos:x}-{n}"));
        stdfs::create_dir(&path)?;
        Ok(Self { path })
    }

    /// Returns the absolute path to the temporary directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Consumes the wrapper without removing the directory. Used
    /// when ownership transfers (the new owner is responsible for
    /// cleanup).
    #[must_use]
    pub fn into_path(mut self) -> PathBuf {
        let out = std::mem::take(&mut self.path);
        // Prevent Drop from removing the directory.
        std::mem::forget(self);
        out
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = stdfs::remove_dir_all(&self.path);
        }
    }
}

/// Creates a freshly-named temporary file under the system temp
/// root and returns `(File, PathBuf)`. The caller is responsible
/// for removing the file when finished - pair with [`TempDir`] for
/// automatic cleanup. Mirrors Python's `tempfile.mkstemp` (sans
/// the file-descriptor return).
pub fn temp_file(prefix: &str) -> io::Result<(File, PathBuf)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut path = std::env::temp_dir();
    path.push(format!("gossamer-{prefix}-{pid}-{nanos:x}-{n}"));
    let file = stdfs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .read(true)
        .open(&path)?;
    Ok((file, path))
}

#[cfg(test)]
fn drain_for(rx: &Receiver<Event>, deadline: std::time::Duration) -> Vec<Event> {
    let mut out = Vec::new();
    let start = std::time::Instant::now();
    while start.elapsed() < deadline {
        if let Ok(ev) = rx.recv_timeout(std::time::Duration::from_millis(50)) {
            out.push(ev);
        }
    }
    out
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

    #[test]
    fn hard_link_creates_second_name_for_same_bytes() {
        let dir = scratch("hardlink-ok");
        let src = dir.join("src.txt");
        let dst = dir.join("dst.txt");
        write(&src, "linked").unwrap();
        hard_link(&src, &dst).unwrap();
        assert!(exists(&dst));
        assert_eq!(read_to_string(&dst).unwrap(), "linked");
        let _ = remove_all(&dir);
    }

    #[test]
    fn hard_link_fails_for_missing_source() {
        let dir = scratch("hardlink-missing");
        stdfs::create_dir_all(&dir).unwrap();
        let err = hard_link(dir.join("nope.txt"), dir.join("dst.txt")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        let _ = remove_all(&dir);
    }

    #[test]
    fn hard_link_fails_when_dst_exists() {
        let dir = scratch("hardlink-dup");
        let src = dir.join("src.txt");
        let dst = dir.join("dst.txt");
        write(&src, "a").unwrap();
        write(&dst, "b").unwrap();
        assert!(hard_link(&src, &dst).is_err());
        let _ = remove_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn set_permissions_mode_sets_unix_bits() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("perm-ok");
        let path = dir.join("file.sh");
        write(&path, "#!/bin/sh\n").unwrap();
        set_permissions_mode(&path, 0o755).unwrap();
        let mode = metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        let _ = remove_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn set_permissions_mode_changes_to_readonly() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("perm-ro");
        let path = dir.join("file.txt");
        write(&path, "ro").unwrap();
        set_permissions_mode(&path, 0o400).unwrap();
        let mode = metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o400);
        // Restore write bit so cleanup can succeed.
        set_permissions_mode(&path, 0o644).unwrap();
        let _ = remove_all(&dir);
    }

    #[test]
    fn set_permissions_mode_fails_for_missing_path() {
        let dir = scratch("perm-missing");
        stdfs::create_dir_all(&dir).unwrap();
        let err = set_permissions_mode(dir.join("nope.txt"), 0o644).unwrap_err();
        #[cfg(unix)]
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        #[cfg(not(unix))]
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        let _ = remove_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn chown_minus_one_leaves_owner_unchanged() {
        use std::os::unix::fs::MetadataExt;
        let dir = scratch("chown-noop");
        let path = dir.join("file.txt");
        write(&path, "x").unwrap();
        let before = metadata(&path).unwrap();
        chown(&path, -1, -1).unwrap();
        let after = metadata(&path).unwrap();
        assert_eq!(before.uid(), after.uid());
        assert_eq!(before.gid(), after.gid());
        let _ = remove_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn chown_to_current_uid_gid_round_trips() {
        use std::os::unix::fs::MetadataExt;
        let dir = scratch("chown-self");
        let path = dir.join("file.txt");
        write(&path, "x").unwrap();
        let md = metadata(&path).unwrap();
        let uid = i64::from(md.uid());
        let gid = i64::from(md.gid());
        chown(&path, uid, gid).unwrap();
        let after = metadata(&path).unwrap();
        assert_eq!(i64::from(after.uid()), uid);
        assert_eq!(i64::from(after.gid()), gid);
        let _ = remove_all(&dir);
    }

    #[test]
    fn chown_fails_for_missing_path() {
        let dir = scratch("chown-missing");
        stdfs::create_dir_all(&dir).unwrap();
        assert!(chown(dir.join("nope.txt"), -1, -1).is_err());
        let _ = remove_all(&dir);
    }

    #[test]
    fn write_atomic_writes_exact_bytes() {
        let dir = scratch("atomic-write");
        let path = dir.join("config.bin");
        let payload: Vec<u8> = (0u8..=255u8).collect();
        write_atomic(&path, &payload).unwrap();
        let read_back = read(&path).unwrap();
        assert_eq!(read_back, payload);
        let _ = remove_all(&dir);
    }

    #[test]
    fn write_atomic_leaves_no_temp_file_after_success() {
        let dir = scratch("atomic-cleanup");
        let path = dir.join("config.toml");
        write_atomic(&path, b"[section]\nkey = 1\n").unwrap();
        let mut found_temp = false;
        for entry in read_dir(&dir).unwrap() {
            if entry.name.contains(".tmp.") {
                found_temp = true;
            }
        }
        assert!(!found_temp, "leftover temp file in {dir:?}");
        let _ = remove_all(&dir);
    }

    #[test]
    fn write_atomic_overwrites_existing_atomically() {
        let dir = scratch("atomic-overwrite");
        let path = dir.join("file.txt");
        write(&path, "old contents").unwrap();
        write_atomic(&path, b"new contents").unwrap();
        assert_eq!(read_to_string(&path).unwrap(), "new contents");
        let _ = remove_all(&dir);
    }

    #[test]
    fn write_atomic_creates_missing_parents() {
        let dir = scratch("atomic-parents");
        let path = dir.join("nested/deeper/file.txt");
        write_atomic(&path, b"deep").unwrap();
        assert_eq!(read_to_string(&path).unwrap(), "deep");
        let _ = remove_all(&dir);
    }

    #[test]
    fn watch_injected_event_arrives_on_receiver() {
        let watcher = Watcher::new().unwrap();
        let rx = watcher.events().expect("first events() yields receiver");
        watcher.inject_for_test(Event {
            path: "/tmp/x".to_string(),
            kind: EventKind::Created,
        });
        let events = drain_for(&rx, std::time::Duration::from_millis(200));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].path, "/tmp/x");
        assert_eq!(events[0].kind, EventKind::Created);
    }

    #[test]
    fn watch_events_taken_only_once() {
        let watcher = Watcher::new().unwrap();
        let _ = watcher.events().expect("first call returns Some");
        assert!(watcher.events().is_none());
    }

    #[test]
    fn watch_add_fails_for_missing_path() {
        let watcher = Watcher::new().unwrap();
        let err = watcher.add("/this/path/does/not/exist/zzz").unwrap_err();
        assert!(matches!(
            err.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::Other
        ));
    }

    #[test]
    fn watch_detects_file_creation() {
        let dir = scratch("watch-create");
        stdfs::create_dir_all(&dir).unwrap();
        let watcher = Watcher::new().unwrap();
        let rx = watcher.events().unwrap();
        watcher.add(&dir.to_string_lossy()).unwrap();
        // Give the platform backend a moment to register the watch.
        // There is no synchronous "watch active" ack from FSEvents /
        // ReadDirectoryChangesW / inotify, so a brief settle before the
        // triggering write is the documented platform constraint.
        std::thread::sleep(std::time::Duration::from_millis(150));
        let file = dir.join("created.txt");
        stdfs::write(&file, b"hello").unwrap();
        // Wait until the matching event arrives, returning as soon as it
        // does. A generous deadline absorbs FSEvents coalescing latency
        // on a loaded CI runner without slowing the common (fast) case.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut saw = false;
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(e) if e.path.ends_with("created.txt") && e.kind == EventKind::Created => {
                    saw = true;
                    break;
                }
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        assert!(saw, "expected Created event for created.txt within 10s");
        drop(watcher);
        let _ = remove_all(&dir);
    }

    #[test]
    fn watch_event_kind_translation_covers_each_arm() {
        let watcher = Watcher::new().unwrap();
        let rx = watcher.events().unwrap();
        watcher.inject_for_test(Event {
            path: "a".into(),
            kind: EventKind::Created,
        });
        watcher.inject_for_test(Event {
            path: "b".into(),
            kind: EventKind::Modified,
        });
        watcher.inject_for_test(Event {
            path: "c".into(),
            kind: EventKind::Removed,
        });
        let events = drain_for(&rx, std::time::Duration::from_millis(300));
        let kinds: Vec<EventKind> = events.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![EventKind::Created, EventKind::Modified, EventKind::Removed]
        );
    }

    #[test]
    fn mmap_read_round_trips_bytes() {
        let dir = scratch("mmap-rt");
        let path = dir.join("data.bin");
        let payload: Vec<u8> = (0u8..=255u8).collect();
        write(&path, &payload).unwrap();
        let mm = mmap_read(&path.to_string_lossy()).unwrap();
        assert_eq!(mm.len(), payload.len());
        assert_eq!(mm.as_slice(), payload.as_slice());
        drop(mm);
        let _ = remove_all(&dir);
    }

    #[test]
    fn mmap_read_rejects_empty_file() {
        let dir = scratch("mmap-empty");
        let path = dir.join("empty.bin");
        write(&path, b"").unwrap();
        let err = mmap_read(&path.to_string_lossy()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let _ = remove_all(&dir);
    }

    #[test]
    fn mmap_read_fails_for_missing_path() {
        let dir = scratch("mmap-missing");
        stdfs::create_dir_all(&dir).unwrap();
        let err = mmap_read(&dir.join("nope.bin").to_string_lossy()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        let _ = remove_all(&dir);
    }

    #[test]
    fn mmap_read_is_empty_is_false_for_nonempty() {
        let dir = scratch("mmap-empty-false");
        let path = dir.join("x.bin");
        write(&path, b"abc").unwrap();
        let mm = mmap_read(&path.to_string_lossy()).unwrap();
        assert!(!mm.is_empty());
        assert_eq!(mm.len(), 3);
        drop(mm);
        let _ = remove_all(&dir);
    }

    #[test]
    fn mmap_read_large_file_maps_full_length() {
        let dir = scratch("mmap-large");
        let path = dir.join("big.bin");
        let payload = vec![0x42u8; 64 * 1024];
        write(&path, &payload).unwrap();
        let mm = mmap_read(&path.to_string_lossy()).unwrap();
        assert_eq!(mm.len(), payload.len());
        assert_eq!(mm.as_slice()[0], 0x42);
        assert_eq!(mm.as_slice()[payload.len() - 1], 0x42);
        drop(mm);
        let _ = remove_all(&dir);
    }

    #[test]
    fn lock_exclusive_then_unlock_round_trips() {
        let dir = scratch("lock-rt");
        let path = dir.join("a.txt");
        write(&path, "x").unwrap();
        let f = File::open(&path).unwrap();
        lock_exclusive(&f).unwrap();
        unlock(&f).unwrap();
        drop(f);
        let _ = remove_all(&dir);
    }

    #[test]
    fn try_lock_exclusive_second_holder_fails() {
        let dir = scratch("lock-conflict");
        let path = dir.join("a.txt");
        write(&path, "x").unwrap();
        let f1 = File::open(&path).unwrap();
        let f2 = File::open(&path).unwrap();
        lock_exclusive(&f1).unwrap();
        let err = try_lock_exclusive(&f2).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        unlock(&f1).unwrap();
        drop(f1);
        drop(f2);
        let _ = remove_all(&dir);
    }

    #[test]
    fn try_lock_shared_two_holders_succeed() {
        let dir = scratch("lock-shared");
        let path = dir.join("a.txt");
        write(&path, "x").unwrap();
        let f1 = File::open(&path).unwrap();
        let f2 = File::open(&path).unwrap();
        try_lock_shared(&f1).unwrap();
        try_lock_shared(&f2).unwrap();
        unlock(&f1).unwrap();
        unlock(&f2).unwrap();
        drop(f1);
        drop(f2);
        let _ = remove_all(&dir);
    }

    #[test]
    fn try_lock_exclusive_then_shared_conflicts() {
        let dir = scratch("lock-x-then-s");
        let path = dir.join("a.txt");
        write(&path, "x").unwrap();
        let f1 = File::open(&path).unwrap();
        let f2 = File::open(&path).unwrap();
        lock_exclusive(&f1).unwrap();
        let err = try_lock_shared(&f2).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        unlock(&f1).unwrap();
        drop(f1);
        drop(f2);
        let _ = remove_all(&dir);
    }

    #[test]
    fn lock_released_after_drop_or_unlock_can_be_reacquired() {
        let dir = scratch("lock-reacquire");
        let path = dir.join("a.txt");
        write(&path, "x").unwrap();
        {
            let f = File::open(&path).unwrap();
            lock_exclusive(&f).unwrap();
            unlock(&f).unwrap();
        }
        let f = File::open(&path).unwrap();
        try_lock_exclusive(&f).unwrap();
        unlock(&f).unwrap();
        drop(f);
        let _ = remove_all(&dir);
    }
}
