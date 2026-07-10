//! Streaming fs file handle runtime tests.

use std::ffi::CString;

use gossamer_runtime::c_abi::{
    gos_rt_fs_file_close, gos_rt_fs_file_create, gos_rt_fs_file_flush, gos_rt_fs_file_open,
    gos_rt_fs_file_read_to_string, gos_rt_fs_file_write, gos_rt_result_disc, gos_rt_result_payload,
    gos_rt_vec_free, gos_rt_vec_with_capacity,
};

fn scratch_path(tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("gos-runtime-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn file_handle_write_flush_close_and_read_to_string_round_trip() {
    let path = scratch_path("file-handle");
    let cpath = CString::new(path.to_string_lossy().as_bytes()).unwrap();
    // SAFETY: C strings and runtime vectors are live for each call; handles
    // are closed once the assertions finish.
    unsafe {
        let created = gos_rt_fs_file_create(cpath.as_ptr());
        assert_eq!(gos_rt_result_disc(created), 0);
        let writer = gos_rt_result_payload(created);

        let bytes = b"streamed";
        let v = gos_rt_vec_with_capacity(1, bytes.len() as i64);
        (*v).len = bytes.len() as i64;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), (*v).ptr.as_ptr(), bytes.len());
        let written = gos_rt_fs_file_write(writer, v);
        assert_eq!(gos_rt_result_disc(written), 0);
        assert_eq!(gos_rt_result_payload(written), bytes.len() as i64);
        assert_eq!(gos_rt_result_disc(gos_rt_fs_file_flush(writer)), 0);
        gos_rt_vec_free(v);
        gos_rt_fs_file_close(writer);

        let opened = gos_rt_fs_file_open(cpath.as_ptr());
        assert_eq!(gos_rt_result_disc(opened), 0);
        let reader = gos_rt_result_payload(opened);
        let text = gos_rt_fs_file_read_to_string(reader);
        assert_eq!(gos_rt_result_disc(text), 0);
        let raw = gos_rt_result_payload(text) as *const std::os::raw::c_char;
        assert_eq!(std::ffi::CStr::from_ptr(raw).to_str().unwrap(), "streamed");
        gos_rt_fs_file_close(reader);
    }
    let _ = std::fs::remove_file(path);
}
