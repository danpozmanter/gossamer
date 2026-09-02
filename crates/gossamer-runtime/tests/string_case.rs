//! String case-conversion runtime tests.

use std::ffi::{CStr, CString};

use gossamer_runtime::c_abi::{
    gos_rt_str_append_bytes, gos_rt_str_free, gos_rt_str_free_typed, gos_rt_str_len,
    gos_rt_str_retain_typed, gos_rt_str_to_upper, gos_rt_str_with_capacity,
};

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
fn string_with_capacity_reuses_its_unique_buffer() {
    unsafe {
        let string = gos_rt_str_with_capacity(128);
        let original = string;
        let appended = gos_rt_str_append_bytes(string, b"hello".as_ptr(), 5);
        assert_eq!(appended, original, "reserved buffer should grow in place");
        assert_eq!(gos_rt_str_len(appended), 5);
        gos_rt_str_free(appended);
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

#[test]
fn foreign_heap_bytes_shaped_like_a_body_are_never_treated_as_a_runtime_string() {
    // A pointer five bytes into a heap allocation has the low-bit shape of a
    // runtime string body, and the bytes before it are readable heap memory
    // rather than an owner. The untyped entry points must leave it alone:
    // no count is touched and nothing is freed.
    // Heap memory, so the bytes before the body are readable and the owner
    // check is what rejects it.
    let mut block: Box<[u8]> = Box::new([0u8; 64]);
    block[5..14].copy_from_slice(b"borrowed\0");
    let body = block[5..].as_mut_ptr().cast::<std::ffi::c_char>();
    unsafe {
        gos_rt_str_retain_typed(body);
        gos_rt_str_free(body);
        gos_rt_str_free_typed(body);
        assert_eq!(gos_rt_str_len(body), 8);
    }
    assert_eq!(&block[5..13], b"borrowed");
    assert!(block[..5].iter().all(|&b| b == 0));
}
