//! Where a relative path points while a `comptime` block is being folded.
//!
//! An embedded asset belongs to the source that embeds it, so
//! `fs::read_to_string("templates/index.html")` inside a `comptime fn`
//! must mean the same file whatever directory the build was started from.
//! Resolving against the process working directory instead makes the same
//! program embed different bytes depending on where `gos build` ran, which
//! is the one thing an embed must never do.
//!
//! The anchor is thread-local and only set while folding: at run time a
//! relative path means what it always did, relative to the process
//! working directory.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

thread_local! {
    /// Directory a relative path resolves against, or `None` outside a
    /// fold.
    static ANCHOR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Anchors relative paths at `dir` until the guard is dropped.
pub struct Anchored(Option<PathBuf>);

impl Anchored {
    /// Anchors at the directory holding `source`, which may be a file or a
    /// directory path. A source with no directory part anchors nowhere and
    /// leaves resolution as it was.
    #[must_use]
    pub fn at_source(source: &str) -> Self {
        let dir = Path::new(source)
            .parent()
            .filter(|p| !p.as_os_str().is_empty());
        let previous = ANCHOR.with(|cell| cell.replace(dir.map(Path::to_path_buf)));
        Self(previous)
    }
}

impl Drop for Anchored {
    fn drop(&mut self) {
        let previous = self.0.take();
        ANCHOR.with(|cell| *cell.borrow_mut() = previous);
    }
}

/// `path` as it should be opened: joined onto the anchor when one is set
/// and the path is relative, unchanged otherwise.
///
/// The answer is absolute whenever it anchors, so a path a directory
/// listing hands back - already under the anchor - is not anchored a
/// second time when it is read.
#[must_use]
pub fn resolve(path: &str) -> String {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return path.to_string();
    }
    ANCHOR.with(|cell| {
        cell.borrow().as_ref().map_or_else(
            || path.to_string(),
            |dir| {
                let anchored = dir.join(candidate);
                if anchored.is_absolute() {
                    return anchored.to_string_lossy().into_owned();
                }
                std::env::current_dir()
                    .map(|cwd| cwd.join(&anchored))
                    .unwrap_or(anchored)
                    .to_string_lossy()
                    .into_owned()
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This platform's absolute root: `/` on Unix, the current drive's
    /// `C:\` on Windows. A path literal cannot stand in for it - a
    /// leading `/` is root-relative on Windows, not absolute.
    fn root() -> PathBuf {
        std::env::current_dir()
            .expect("current directory")
            .ancestors()
            .last()
            .expect("every path has a root ancestor")
            .to_path_buf()
    }

    /// `root()/a/b/...`, spelled with this platform's separator.
    fn under_root(parts: &[&str]) -> PathBuf {
        parts.iter().fold(root(), |acc, part| acc.join(part))
    }

    fn anchor_at(parts: &[&str]) -> Anchored {
        Anchored::at_source(&under_root(parts).to_string_lossy())
    }

    #[test]
    fn a_relative_path_resolves_against_the_source_directory() {
        let _guard = anchor_at(&["project", "src", "app.gos"]);
        assert_eq!(
            resolve("templates/index.html"),
            under_root(&["project", "src"])
                .join("templates/index.html")
                .to_string_lossy()
        );
    }

    #[test]
    fn an_anchored_answer_is_absolute_so_it_never_anchors_twice() {
        let _guard = Anchored::at_source("relative/dir/app.gos");
        let once = resolve("assets/x.css");
        assert!(Path::new(&once).is_absolute(), "got {once}");
        // A listing hands its entries back to a read; the second pass must
        // leave them where they are.
        assert_eq!(resolve(&once), once);
    }

    #[test]
    fn an_absolute_path_is_left_alone() {
        let _guard = anchor_at(&["project", "src", "app.gos"]);
        let absolute = under_root(&["etc", "hosts"]).to_string_lossy().into_owned();
        assert_eq!(resolve(&absolute), absolute);
    }

    #[test]
    fn outside_a_fold_a_relative_path_is_unchanged() {
        assert_eq!(resolve("asset.txt"), "asset.txt");
    }

    #[test]
    fn the_anchor_is_restored_when_the_guard_ends() {
        {
            let _outer = anchor_at(&["a", "one.gos"]);
            {
                let _inner = anchor_at(&["b", "two.gos"]);
                assert_eq!(resolve("x"), under_root(&["b", "x"]).to_string_lossy());
            }
            assert_eq!(resolve("x"), under_root(&["a", "x"]).to_string_lossy());
        }
        assert_eq!(resolve("x"), "x");
    }
}
