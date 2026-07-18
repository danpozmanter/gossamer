//! String append runtime tests.

use std::ffi::{CStr, CString};

use gossamer_runtime::c_abi::{
    alloc_cstring, gos_rt_arena_pop, gos_rt_arena_push, gos_rt_str_append_bytes,
    gos_rt_str_append_i64, gos_rt_str_concat_drop_a, gos_rt_str_free, gos_rt_str_with_capacity,
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
        let promoted = gos_rt_str_append_bytes(arena_string, b"escaped".as_ptr(), 7);
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
