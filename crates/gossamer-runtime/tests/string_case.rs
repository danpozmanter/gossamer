//! String case-conversion runtime tests.

use std::ffi::{CStr, CString};

use gossamer_runtime::c_abi::{gos_rt_str_free, gos_rt_str_to_upper};

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
