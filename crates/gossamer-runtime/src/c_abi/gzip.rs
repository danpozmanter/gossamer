#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::same_length_and_capacity)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::ptr_as_ptr)]
#![allow(static_mut_refs)]
#![allow(unused_unsafe)]
#![allow(clippy::wildcard_imports)]

use std::ffi::CStr;
use std::os::raw::c_char;

use super::*;

// ---------------------------------------------------------------
// gzip module — encode / decode using `flate2`.
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_gzip_encode(data: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if data.is_null() {
            return alloc_cstring(b"");
        }
        let bytes = unsafe { CStr::from_ptr(data).to_bytes() };
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        if enc.write_all(bytes).is_err() {
            return alloc_cstring(b"");
        }
        let buf = enc.finish().unwrap_or_default();
        alloc_cstring(&buf)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_gzip_decode(data: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if data.is_null() {
            return alloc_cstring(b"");
        }
        let bytes = unsafe { CStr::from_ptr(data).to_bytes() };
        use std::io::Read;
        let mut dec = flate2::read::GzDecoder::new(bytes);
        let mut out = Vec::new();
        if dec.read_to_end(&mut out).is_err() {
            return alloc_cstring(b"");
        }
        alloc_cstring(&out)
    })
}
