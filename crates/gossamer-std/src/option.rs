//! Chaining combinators for `std::option`.
//!
//! Generic, data-last. Mirrors F#'s `Option` module so
//! `opt |> option::map(f) |> option::default(0)` threads cleanly.
//!
//! The user-facing dispatch in `.gos` programs lives in
//! `crates/gossamer-interp/src/stdlib_builtins.rs::install_option` —
//! these Rust helpers exist for stdlib code that wants to reach the
//! same surface.

#![forbid(unsafe_code)]

/// `Some(f(x))` if `opt` is `Some(x)`, otherwise `None`.
#[must_use]
pub fn map<T, U, F: FnOnce(T) -> U>(f: F, opt: Option<T>) -> Option<U> {
    opt.map(f)
}

/// `f(x)` if `opt` is `Some(x)`, otherwise `None`. F# `Option.bind`.
#[must_use]
pub fn and_then<T, U, F: FnOnce(T) -> Option<U>>(f: F, opt: Option<T>) -> Option<U> {
    opt.and_then(f)
}

/// `Some(x)` if `p(&x)` holds, otherwise `None`.
#[must_use]
pub fn filter<T, P: FnOnce(&T) -> bool>(p: P, opt: Option<T>) -> Option<T> {
    opt.filter(p)
}

/// `x` if `Some(x)`, otherwise `v`. F# `Option.defaultValue`.
pub fn default<T>(v: T, opt: Option<T>) -> T {
    opt.unwrap_or(v)
}

/// `x` if `Some(x)`, otherwise `f()`.
pub fn default_with<T, F: FnOnce() -> T>(f: F, opt: Option<T>) -> T {
    opt.unwrap_or_else(f)
}

/// `opt` if `Some`, otherwise `alt`.
#[must_use]
pub fn or<T>(alt: Option<T>, opt: Option<T>) -> Option<T> {
    opt.or(alt)
}

/// `opt` if `Some`, otherwise `f()`.
#[must_use]
pub fn or_else<T, F: FnOnce() -> Option<T>>(f: F, opt: Option<T>) -> Option<T> {
    opt.or_else(f)
}

/// Run `f(x)` for the side-effect when `opt` is `Some(x)`.
pub fn iter<T, F: FnOnce(T)>(f: F, opt: Option<T>) {
    if let Some(x) = opt {
        f(x);
    }
}

/// `Option<Option<T>>` → `Option<T>`.
#[must_use]
pub fn flatten<T>(opt: Option<Option<T>>) -> Option<T> {
    opt.flatten()
}

/// `Some((a, b))` if both are `Some`, otherwise `None`.
#[must_use]
pub fn zip<A, B>(a: Option<A>, b: Option<B>) -> Option<(A, B)> {
    a.zip(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_some_none() {
        assert_eq!(map(|n: i64| n * 2, Some(5)), Some(10));
        assert_eq!(map(|n: i64| n * 2, None), None);
    }

    #[test]
    fn and_then_flattens() {
        let half = |n: i64| if n % 2 == 0 { Some(n / 2) } else { None };
        assert_eq!(and_then(half, Some(8)), Some(4));
        assert_eq!(and_then(half, Some(7)), None);
        assert_eq!(and_then(half, None), None);
    }

    #[test]
    fn default_falls_back() {
        assert_eq!(default(0, Some(5)), 5);
        assert_eq!(default(0, None::<i64>), 0);
    }

    #[test]
    fn filter_drops_when_false() {
        assert_eq!(filter(|n: &i64| *n > 3, Some(5)), Some(5));
        assert_eq!(filter(|n: &i64| *n > 3, Some(2)), None);
    }

    #[test]
    fn zip_requires_both() {
        assert_eq!(zip(Some(1), Some('a')), Some((1, 'a')));
        assert_eq!(zip(None::<i64>, Some('a')), None);
    }
}
