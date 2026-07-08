//! Chaining combinators for `std::result`.
//!
//! Generic, data-last. Mirrors F#'s `Result` module. The `?`
//! operator (SPEC §4.5) remains the right tool for short-circuit
//! propagation; these combinators handle in-pipeline transformation
//! when the chain doesn't return from the enclosing fn.

#![forbid(unsafe_code)]

/// `Ok(f(x))` if `r` is `Ok(x)`, otherwise the original `Err`.
pub fn map<T, U, E, F: FnOnce(T) -> U>(f: F, r: Result<T, E>) -> Result<U, E> {
    r.map(f)
}

/// `Err(f(e))` if `r` is `Err(e)`, otherwise the original `Ok`.
pub fn map_err<T, E, F, M: FnOnce(E) -> F>(m: M, r: Result<T, E>) -> Result<T, F> {
    r.map_err(m)
}

/// `f(x)` if `r` is `Ok(x)`, otherwise the original `Err`. F# `Result.bind`.
pub fn and_then<T, U, E, F: FnOnce(T) -> Result<U, E>>(f: F, r: Result<T, E>) -> Result<U, E> {
    r.and_then(f)
}

/// `f(e)` if `r` is `Err(e)`, otherwise the original `Ok`.
pub fn or_else<T, E, F, M: FnOnce(E) -> Result<T, F>>(m: M, r: Result<T, E>) -> Result<T, F> {
    r.or_else(m)
}

/// `x` if `Ok(x)`, otherwise `v`.
pub fn unwrap_or<T, E>(v: T, r: Result<T, E>) -> T {
    r.unwrap_or(v)
}

/// `x` if `Ok(x)`, otherwise `f(e)`.
pub fn unwrap_or_else<T, E, F: FnOnce(E) -> T>(f: F, r: Result<T, E>) -> T {
    r.unwrap_or_else(f)
}

/// `Some(x)` if `Ok(x)`, else `None`.
pub fn ok<T, E>(r: Result<T, E>) -> Option<T> {
    r.ok()
}

/// `Some(e)` if `Err(e)`, else `None`.
pub fn err<T, E>(r: Result<T, E>) -> Option<E> {
    r.err()
}

/// True iff `r` is `Ok`.
pub fn is_ok<T, E>(r: &Result<T, E>) -> bool {
    r.is_ok()
}

/// True iff `r` is `Err`.
pub fn is_err<T, E>(r: &Result<T, E>) -> bool {
    r.is_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_ok_err() {
        assert_eq!(map(|n: i64| n + 1, Ok::<_, &str>(4)), Ok(5));
        assert_eq!(map(|n: i64| n + 1, Err::<i64, _>("oops")), Err("oops"));
    }

    #[test]
    fn map_err_changes_e() {
        assert_eq!(
            map_err(|e: &str| e.len(), Err::<i64, _>("oops")),
            Err::<i64, _>(4)
        );
    }

    #[test]
    fn and_then_chains() {
        let half = |n: i64| {
            if n % 2 == 0 {
                Ok::<_, &str>(n / 2)
            } else {
                Err("odd")
            }
        };
        assert_eq!(and_then(half, Ok::<_, &str>(8)), Ok(4));
        assert_eq!(and_then(half, Ok::<_, &str>(7)), Err("odd"));
    }

    #[test]
    fn default_falls_back() {
        assert_eq!(unwrap_or(0, Ok::<_, &str>(5)), 5);
        assert_eq!(unwrap_or(0, Err::<i64, _>("bad")), 0);
    }

    #[test]
    fn ok_err_extract() {
        assert_eq!(ok(Ok::<_, &str>(5)), Some(5));
        assert_eq!(ok(Err::<i64, _>("bad")), None);
        assert_eq!(err(Err::<i64, _>("bad")), Some("bad"));
        assert_eq!(err(Ok::<_, &str>(5)), None);
    }
}
