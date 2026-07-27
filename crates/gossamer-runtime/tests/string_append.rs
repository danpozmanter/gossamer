//! String append runtime tests.

use std::ffi::{CStr, CString};

use gossamer_runtime::c_abi::{
    alloc_cstring, gos_rt_arena_pop, gos_rt_arena_push, gos_rt_str_append_bytes,
    gos_rt_str_append_i64, gos_rt_str_concat_drop_a, gos_rt_str_free, gos_rt_str_push_byte,
    gos_rt_str_push_char, gos_rt_str_with_capacity,
};

#[test]
fn append_i64_formats_into_existing_builder() {
    let prefix = CString::new("id=").expect("literal has no nul");
    unsafe {
        let first = gos_rt_str_append_i64(prefix.as_ptr(), -42);
        let second = gos_rt_str_append_i64(first, 17);
        let out = CStr::from_ptr(second).to_str().unwrap().to_owned();
        gos_rt_str_free(second);
        assert_eq!(out, "id=-4217");
    }
}

#[test]
fn growable_string_survives_the_arena_that_created_it() {
    unsafe {
        gos_rt_arena_push();
        let arena_string = gos_rt_str_with_capacity(64);
        // Growable strings still use the arena fast path when they have no
        // arena-backed source. The ABI tag lives immediately before content.
        assert_eq!(*arena_string.cast::<u8>().sub(1), 0xAA);
        let promoted = gos_rt_str_append_bytes(arena_string, b"escaped".as_ptr(), 7);
        // Copying arena storage promotes the result before the slab can be
        // recycled over its source bytes.
        assert_eq!(*promoted.cast::<u8>().sub(1), 0xAB);
        gos_rt_arena_pop();

        assert_eq!(CStr::from_ptr(promoted).to_bytes(), b"escaped");
        gos_rt_str_free(promoted);
    }
}

#[test]
fn concat_drop_a_appends_header_backed_fragment() {
    let prefix = CString::new("name=").expect("literal has no nul");
    unsafe {
        let fragment = alloc_cstring(b"user-000042");
        let out_ptr = gos_rt_str_concat_drop_a(prefix.as_ptr(), fragment);
        let out = CStr::from_ptr(out_ptr).to_str().unwrap().to_owned();
        gos_rt_str_free(fragment);
        gos_rt_str_free(out_ptr);
        assert_eq!(out, "name=user-000042");
    }
}

#[test]
fn character_pushes_reuse_unique_reserved_storage() {
    unsafe {
        let string = gos_rt_str_with_capacity(16);
        let after_char = gos_rt_str_push_char(string, 'a' as i32);
        assert_eq!(after_char, string, "reserved push should stay in place");
        let after_unicode = gos_rt_str_push_char(after_char, 'ç' as i32);
        assert_eq!(
            after_unicode, string,
            "multibyte push should reuse remaining capacity"
        );
        let after_byte = gos_rt_str_push_byte(after_unicode, i32::from(b'!'));
        assert_eq!(after_byte, string, "byte push should stay in place");
        assert_eq!(CStr::from_ptr(after_byte).to_bytes(), "aç!".as_bytes());
        gos_rt_str_free(after_byte);
    }
}

#[test]
fn character_push_grows_an_exhausted_buffer() {
    unsafe {
        let string = alloc_cstring(b"full");
        let grown = gos_rt_str_push_char(string, '!' as i32);
        assert_ne!(grown, string, "an exhausted buffer must be replaced");
        assert_eq!(CStr::from_ptr(grown).to_bytes(), b"full!");
        gos_rt_str_free(grown);
    }
}
