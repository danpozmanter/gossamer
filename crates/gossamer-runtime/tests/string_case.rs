//! String case-conversion runtime tests.

use std::ffi::{CStr, CString};

use gossamer_runtime::c_abi::{gos_rt_str_free, gos_rt_str_len, gos_rt_str_to_upper};

fn upper(input: &str) -> String {
    let input = CString::new(input).expect("test input must not contain NUL");
    // SAFETY: `input` is a live NUL-terminated string for the duration of the
    // call, and the returned runtime string is freed with `gos_rt_str_free`.
    unsafe {
        let raw = gos_rt_str_to_upper(input.as_ptr());
        let out = CStr::from_ptr(raw).to_str().unwrap().to_owned();
        gos_rt_str_free(raw);
        out
    }
}

#[test]
fn str_to_upper_ascii_fast_path_matches_unicode_contract() {
    assert_eq!(upper("json-tag_42"), "JSON-TAG_42");
}

#[test]
fn str_to_upper_unicode_fallback_preserves_expansion() {
    assert_eq!(upper("straße"), "STRASSE");
}

#[test]
fn foreign_cstring_is_never_prefix_probed_by_public_string_helpers() {
    // Miri rejects the old `ptr[-1]` provenance probe because `foreign` owns
    // exactly its C-string bytes. The public ABI must treat it as borrowed:
    // length can use `strlen`, while free is a no-op.
    let foreign = CString::new("borrowed").unwrap();
    unsafe {
        assert_eq!(gos_rt_str_len(foreign.as_ptr()), 8);
        gos_rt_str_free(foreign.as_ptr().cast_mut());
    }
    assert_eq!(foreign.to_str().unwrap(), "borrowed");
}
