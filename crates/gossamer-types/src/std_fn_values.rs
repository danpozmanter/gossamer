//! Std free functions that can be passed as first-class values.
//!
//! The VM represents every std builtin as a callable `Value::Builtin`,
//! so `r.map_err(errors::new)` works there for any name. The compiled
//! tiers need a concrete C-ABI symbol to take the address of, so only
//! the names tabled here are supported as values; everything else is
//! rejected uniformly by the checker (GT0015) so the program behaves
//! the same on every tier.
//!
//! Every entry maps a source path to a `gos_rt_*` shim whose C ABI is
//! word-shaped (pointer/i64 arguments, word or packed-i128 return), so
//! the per-shape callable thunks can forward to it directly - the
//! eta-expansion is the existing env-blob + `__fn_thunk_*` machinery,
//! pointed at the runtime symbol instead of a nonexistent
//! `module::name` function.

#![forbid(unsafe_code)]

/// Type shape of one parameter or return slot of a tabled std fn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdValTy {
    /// `String` (c-string pointer at the ABI level).
    Str,
    /// `i64`.
    I64,
    /// `errors::Error` (opaque pointer).
    Error,
    /// `Result<i64, errors::Error>` (packed i128).
    ResultI64,
}

/// One std free function usable as a first-class value.
#[derive(Debug, Clone, Copy)]
pub struct StdFnValue {
    /// Canonical source path, e.g. `errors::new`.
    pub path: &'static str,
    /// Runtime symbol whose address implements the function natively.
    pub rt_symbol: &'static str,
    /// Parameter shapes.
    pub params: &'static [StdValTy],
    /// Return shape.
    pub ret: StdValTy,
}

/// Std fns supported as first-class values on every tier. Keep sorted
/// by path.
pub const STD_FN_VALUES: &[StdFnValue] = &[
    StdFnValue {
        path: "errors::new",
        rt_symbol: "gos_rt_error_new",
        params: &[StdValTy::Str],
        ret: StdValTy::Error,
    },
    StdFnValue {
        path: "strconv::format_i64",
        rt_symbol: "gos_rt_strconv_format_i64",
        params: &[StdValTy::I64],
        ret: StdValTy::Str,
    },
    StdFnValue {
        path: "strconv::parse_i64",
        rt_symbol: "gos_rt_strconv_parse_i64",
        params: &[StdValTy::Str],
        ret: StdValTy::ResultI64,
    },
    StdFnValue {
        path: "strings::to_lowercase",
        rt_symbol: "gos_rt_str_to_lower",
        params: &[StdValTy::Str],
        ret: StdValTy::Str,
    },
    StdFnValue {
        path: "strings::to_uppercase",
        rt_symbol: "gos_rt_str_to_upper",
        params: &[StdValTy::Str],
        ret: StdValTy::Str,
    },
    StdFnValue {
        path: "strings::trim",
        rt_symbol: "gos_rt_str_trim",
        params: &[StdValTy::Str],
        ret: StdValTy::Str,
    },
    StdFnValue {
        path: "strings::trim_end",
        rt_symbol: "gos_rt_str_trim_end",
        params: &[StdValTy::Str],
        ret: StdValTy::Str,
    },
    StdFnValue {
        path: "strings::trim_start",
        rt_symbol: "gos_rt_str_trim_start",
        params: &[StdValTy::Str],
        ret: StdValTy::Str,
    },
];

/// Table entry for `path` (canonical `module::name`, no `std::`
/// prefix), or `None` when the fn is not supported as a value.
#[must_use]
pub fn std_fn_value(path: &str) -> Option<&'static StdFnValue> {
    STD_FN_VALUES.iter().find(|e| e.path == path)
}

/// Runtime symbol for a std fn path used as a value, accepting both
/// `module::name` and `std::module::name` spellings.
#[must_use]
pub fn rt_symbol_for_std_fn(joined: &str) -> Option<&'static str> {
    let canonical = joined.strip_prefix("std::").unwrap_or(joined);
    std_fn_value(canonical).map(|e| e.rt_symbol)
}

/// Whether `segments` names a free function the stdlib exports: a
/// module-qualified, all-lowercase path the manifest lists. A leading
/// capital is a type, so `Map::new` is an associated function rather
/// than a free one and is not this shape.
#[must_use]
pub fn is_std_free_fn_path(segments: &[&str]) -> bool {
    let stripped: &[&str] = match segments {
        ["std", rest @ ..] => rest,
        other => other,
    };
    if stripped.len() < 2
        || !stripped
            .iter()
            .all(|seg| seg.chars().next().is_some_and(char::is_lowercase))
    {
        return false;
    }
    gossamer_resolve::is_stdlib_item_path(&stripped.join("::"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_by_path() {
        for pair in STD_FN_VALUES.windows(2) {
            assert!(
                pair[0].path < pair[1].path,
                "{} >= {}",
                pair[0].path,
                pair[1].path
            );
        }
    }

    #[test]
    fn lookup_accepts_std_prefix() {
        assert_eq!(
            rt_symbol_for_std_fn("errors::new"),
            Some("gos_rt_error_new")
        );
        assert_eq!(
            rt_symbol_for_std_fn("std::strings::to_uppercase"),
            Some("gos_rt_str_to_upper")
        );
        assert_eq!(rt_symbol_for_std_fn("strings::nope"), None);
    }

    #[test]
    fn shape_check_rejects_consts_and_user_paths() {
        assert!(is_std_free_fn_path(&["errors", "new"]));
        assert!(is_std_free_fn_path(&["std", "strings", "to_uppercase"]));
        assert!(is_std_free_fn_path(&["encoding", "base64", "encode"]));
        assert!(!is_std_free_fn_path(&["math", "PI"]));
        assert!(!is_std_free_fn_path(&["mymod", "helper"]));
        assert!(!is_std_free_fn_path(&["errors"]));
        assert!(!is_std_free_fn_path(&["Map", "new"]));
    }
}
