//! Paths a compile-time region read, recorded so a build can hash them.
//!
//! A `comptime` region that reads a file makes that file an input of
//! the build exactly as the source text is: the same source compiled
//! against different bytes is a different program. The set is not
//! knowable before the fold - which file a region reads is decided by
//! running it - so it is discovered while the fold runs and handed to
//! whoever is deciding whether an artifact is still current, the way a
//! C compiler's dependency file carries the headers it opened.
//!
//! Recording is process-wide rather than thread-local because a
//! compile-time goroutine folds on whatever worker thread it lands on,
//! and it is live only while a fold is in progress, so a program's own
//! run-time reads are never recorded.

use std::collections::BTreeSet;
use std::path::PathBuf;

use parking_lot::Mutex;

use crate::comptime_policy::Confined;

/// Paths read by the fold in progress.
static RECORDED: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// Starts a fresh recording, discarding whatever a previous fold left.
///
/// Called by the fold itself, so a caller that never asks for the set
/// keeps at most one fold's worth of paths.
pub fn begin() {
    RECORDED.lock().clear();
}

/// Records `path` as an input of the fold in progress.
///
/// A no-op outside a fold, which is what keeps a program's own reads
/// out of the set.
pub fn record(path: &str) {
    if !Confined::active() {
        return;
    }
    RECORDED.lock().insert(absolute(path));
}

/// Records every path in `paths`.
pub fn record_each<S: AsRef<str>>(paths: impl IntoIterator<Item = S>) {
    if !Confined::active() {
        return;
    }
    let mut recorded = RECORDED.lock();
    for path in paths {
        recorded.insert(absolute(path.as_ref()));
    }
}

/// `path` as an absolute path, so the recorded set names the same
/// files however the build that reads it back was started.
fn absolute(path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir().map_or(path.clone(), |cwd| cwd.join(path))
}

/// Takes the paths recorded so far, sorted and deduplicated.
#[must_use]
pub fn take() -> Vec<PathBuf> {
    std::mem::take(&mut *RECORDED.lock()).into_iter().collect()
}

#[cfg(test)]
mod comptime_inputs_tests {
    use super::*;

    /// Serializes the cases: the recording is process-wide, so two
    /// tests running at once would each see the other's paths.
    static SEQUENCE: Mutex<()> = Mutex::new(());

    #[test]
    fn a_read_outside_a_fold_is_not_an_input() {
        let _order = SEQUENCE.lock();
        begin();
        record("/etc/hosts");
        assert!(take().is_empty());
    }

    #[test]
    fn a_read_during_a_fold_is_recorded_once() {
        let _order = SEQUENCE.lock();
        begin();
        let _fold = Confined::at_root(PathBuf::from("/"));
        record("/project/profiles/one.toml");
        record("/project/profiles/one.toml");
        record("/project/profiles/two.toml");
        assert_eq!(
            take(),
            vec![
                PathBuf::from("/project/profiles/one.toml"),
                PathBuf::from("/project/profiles/two.toml"),
            ]
        );
        assert!(take().is_empty(), "taking the set clears it");
    }

    /// The set is read back by a build that may have been started
    /// from anywhere, so a path is recorded as the file it names, not
    /// as the spelling the region used.
    #[test]
    fn a_relative_read_is_recorded_against_the_working_directory() {
        let _order = SEQUENCE.lock();
        begin();
        let _fold = Confined::at_root(PathBuf::from("/"));
        record("profiles/standard.toml");
        let cwd = std::env::current_dir().expect("a working directory");
        assert_eq!(take(), vec![cwd.join("profiles/standard.toml")]);
    }

    #[test]
    fn a_new_fold_starts_from_nothing() {
        let _order = SEQUENCE.lock();
        begin();
        let _fold = Confined::at_root(PathBuf::from("/"));
        record("/project/first.toml");
        begin();
        record("/project/second.toml");
        assert_eq!(take(), vec![PathBuf::from("/project/second.toml")]);
    }
}
