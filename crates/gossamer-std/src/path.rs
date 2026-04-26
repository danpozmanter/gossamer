//! Runtime support for `std::path` — OS-neutral path manipulation.
//! All helpers operate on `/`-delimited posix-style paths. The
//! [`native`] submodule wraps the posix forms for native separators
//! (backslash on Windows, forward-slash elsewhere); prefer the
//! semantic layer here for paths that live inside the program and
//! [`native`] only at the boundary where a path crosses into an OS
//! call.

#![forbid(unsafe_code)]

/// Joins `base` with `segment`, collapsing duplicate separators and
/// absorbing a leading `/` in `segment`.
#[must_use]
pub fn join(base: &str, segment: &str) -> String {
    if segment.starts_with('/') {
        return segment.to_string();
    }
    if base.is_empty() {
        return segment.to_string();
    }
    let mut out = base.trim_end_matches('/').to_string();
    out.push('/');
    out.push_str(segment.trim_start_matches('/'));
    out
}

/// Splits `path` into a `(directory, file)` pair. The directory never
/// carries a trailing separator unless the path is `/`.
#[must_use]
pub fn split(path: &str) -> (String, String) {
    match path.rfind('/') {
        None => (String::new(), path.to_string()),
        Some(0) => ("/".to_string(), path[1..].to_string()),
        Some(idx) => (path[..idx].to_string(), path[idx + 1..].to_string()),
    }
}

/// Returns the final component of `path` (the file name).
#[must_use]
pub fn base(path: &str) -> String {
    split(path).1
}

/// Returns the directory portion of `path`.
#[must_use]
pub fn dir(path: &str) -> String {
    let (d, _) = split(path);
    if d.is_empty() {
        ".".to_string()
    } else {
        d
    }
}

/// Returns the extension (including the leading `.`) of `path`, or
/// an empty string when none is present.
#[must_use]
pub fn ext(path: &str) -> String {
    let name = base(path);
    match name.rfind('.') {
        Some(0) | None => String::new(),
        Some(idx) => name[idx..].to_string(),
    }
}

/// Cleans `path` in the same sense as Go's `filepath.Clean`:
/// collapses `..` and `.`, strips duplicate slashes, preserves
/// absolute-ness.
#[must_use]
pub fn clean(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|s: &&str| *s != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let mut out = String::new();
    if absolute {
        out.push('/');
    }
    out.push_str(&parts.join("/"));
    if out.is_empty() {
        ".".to_string()
    } else {
        out
    }
}

/// Returns `true` when `path` starts with `/`.
#[must_use]
pub fn is_absolute(path: &str) -> bool {
    path.starts_with('/')
}

/// Returns `true` when `path` references a file inside `prefix`.
#[must_use]
pub fn has_prefix(path: &str, prefix: &str) -> bool {
    let path = clean(path);
    let prefix = clean(prefix);
    if path == prefix {
        return true;
    }
    if prefix.ends_with('/') {
        path.starts_with(&prefix)
    } else {
        let mut candidate = prefix.clone();
        candidate.push('/');
        path.starts_with(&candidate)
    }
}

pub mod native {
    //! Native-separator wrappers around the posix helpers.
    //!
    //! Convert paths at the OS boundary: read the path back out of
    //! the program in posix form, hand a posix form to the helpers
    //! here, pass the returned native form to system calls. Within
    //! the program, stick to posix — it avoids a combinatorial
    //! explosion of separator conversions.
    //!
    //! On Windows the native separator is `\`; everywhere else it
    //! is `/`, and the helpers are near-identity.
    use super::{clean as posix_clean, join as posix_join};

    /// The platform's preferred path separator character.
    #[cfg(windows)]
    pub const SEPARATOR: char = '\\';
    /// The platform's preferred path separator character.
    #[cfg(not(windows))]
    pub const SEPARATOR: char = '/';

    /// Joins two path components using the platform-native
    /// separator. Input components may use either `/` or `\`; output
    /// uses exclusively the native separator.
    #[must_use]
    pub fn join(base: &str, segment: &str) -> String {
        let posix_base = to_posix(base);
        let posix_segment = to_posix(segment);
        to_native(&posix_join(&posix_base, &posix_segment))
    }

    /// Canonicalises `path` into native-separator form with `..` /
    /// `.` collapsed as by [`super::clean`].
    #[must_use]
    pub fn clean(path: &str) -> String {
        to_native(&posix_clean(&to_posix(path)))
    }

    /// Rewrites a path that may use `\` into posix form, so the
    /// semantic-layer helpers can be used on Windows input.
    #[must_use]
    pub fn to_posix(path: &str) -> String {
        if SEPARATOR == '/' {
            return path.to_string();
        }
        path.replace('\\', "/")
    }

    /// Rewrites a posix-form path into native-separator form.
    #[must_use]
    pub fn to_native(path: &str) -> String {
        if SEPARATOR == '/' {
            return path.to_string();
        }
        path.replace('/', "\\")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn to_posix_round_trips_identity_on_non_windows() {
            // On Unix the conversions are pure identity; on Windows
            // they swap separators in both directions. Either way
            // `to_native(to_posix(x)) == x` for the canonical form.
            let original = "a/b/c";
            assert_eq!(to_native(&to_posix(original)), original);
        }

        #[test]
        fn join_uses_native_separator() {
            let joined = join("a", "b");
            assert!(joined.contains(SEPARATOR));
            assert_eq!(joined.matches(SEPARATOR).count(), 1);
        }

        #[test]
        fn clean_collapses_through_native_layer() {
            let cleaned = clean("a/b/../c");
            let expected = if SEPARATOR == '/' { "a/c" } else { "a\\c" };
            assert_eq!(cleaned, expected);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_basic_cases() {
        assert_eq!(join("a", "b"), "a/b");
        assert_eq!(join("a/", "b"), "a/b");
        assert_eq!(join("a", "/b"), "/b");
        assert_eq!(join("", "b"), "b");
        assert_eq!(join("a", ""), "a/");
    }

    #[test]
    fn split_separates_dir_and_file() {
        assert_eq!(split("a/b/c"), ("a/b".to_string(), "c".to_string()));
        assert_eq!(split("/a"), ("/".to_string(), "a".to_string()));
        assert_eq!(split("a"), (String::new(), "a".to_string()));
    }

    #[test]
    fn dir_returns_dot_when_no_separator() {
        assert_eq!(dir("file"), ".");
        assert_eq!(dir("a/file"), "a");
        assert_eq!(dir("/root/x"), "/root");
    }

    #[test]
    fn ext_returns_final_dot_segment() {
        assert_eq!(ext("a/b.gos"), ".gos");
        assert_eq!(ext("a/b.tar.gz"), ".gz");
        assert_eq!(ext("a/file"), "");
        assert_eq!(ext(".hidden"), "");
    }

    #[test]
    fn clean_collapses_double_slash_and_dots() {
        assert_eq!(clean("a//b/./c"), "a/b/c");
        assert_eq!(clean("a/b/../c"), "a/c");
        assert_eq!(clean("/a/b/../../c"), "/c");
        assert_eq!(clean(""), ".");
        assert_eq!(clean("."), ".");
        assert_eq!(clean("../x"), "../x");
    }

    #[test]
    fn has_prefix_is_path_aware() {
        assert!(has_prefix("a/b/c", "a/b"));
        assert!(has_prefix("a/b", "a/b"));
        assert!(!has_prefix("a/bc", "a/b"));
    }
}
