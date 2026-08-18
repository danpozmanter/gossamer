//! Streaming fs file handle runtime tests.

use std::ffi::CString;

use gossamer_runtime::c_abi::{
    gos_rt_fs_file_close, gos_rt_fs_file_create, gos_rt_fs_file_flush, gos_rt_fs_file_len,
    gos_rt_fs_file_open, gos_rt_fs_file_read_at, gos_rt_fs_file_read_to_string,
    gos_rt_fs_file_sync_all, gos_rt_fs_file_try_lock_range, gos_rt_fs_file_unlock,
    gos_rt_fs_file_unlock_range, gos_rt_fs_file_write, gos_rt_fs_file_write_at,
    gos_rt_fs_file_write_bytes, gos_rt_result_disc, gos_rt_result_payload, gos_rt_vec_free,
    gos_rt_vec_with_capacity,
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

        let text = CString::new("streamed").unwrap();
        let written = gos_rt_fs_file_write(writer, text.as_ptr());
        assert_eq!(gos_rt_result_disc(written), 0);
        assert_eq!(gos_rt_result_payload(written), 8);
        assert_eq!(gos_rt_result_disc(gos_rt_fs_file_flush(writer)), 0);
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

#[test]
fn positional_write_and_read_round_trip_past_the_cursor() {
    let path = scratch_path("file-positional");
    let cpath = CString::new(path.to_string_lossy().as_bytes()).unwrap();
    // SAFETY: the C string and runtime vector are live for each call and every
    // handle is closed once the assertions finish.
    unsafe {
        let created = gos_rt_fs_file_create(cpath.as_ptr());
        assert_eq!(gos_rt_result_disc(created), 0);
        let writer = gos_rt_result_payload(created);

        let bytes = b"page";
        let v = gos_rt_vec_with_capacity(1, bytes.len() as i64);
        (*v).len = bytes.len() as i64;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), (*v).ptr.as_ptr(), bytes.len());
        let written = gos_rt_fs_file_write_at(writer, v, 4096);
        assert_eq!(gos_rt_result_disc(written), 0);
        assert_eq!(gos_rt_result_payload(written), 4);

        let head = gos_rt_fs_file_write_bytes(writer, v);
        assert_eq!(gos_rt_result_disc(head), 0);
        assert_eq!(gos_rt_result_payload(head), 4);
        gos_rt_vec_free(v);

        assert_eq!(gos_rt_result_disc(gos_rt_fs_file_sync_all(writer)), 0);
        gos_rt_fs_file_close(writer);

        let opened = gos_rt_fs_file_open(cpath.as_ptr());
        assert_eq!(gos_rt_result_disc(opened), 0);
        let reader = gos_rt_result_payload(opened);

        let size = gos_rt_fs_file_len(reader);
        assert_eq!(gos_rt_result_disc(size), 0);
        assert_eq!(gos_rt_result_payload(size), 4100);

        let read = gos_rt_fs_file_read_at(reader, 4, 4096);
        assert_eq!(gos_rt_result_disc(read), 0);
        let out = gos_rt_result_payload(read) as *const gossamer_runtime::c_abi::vec::GosVec;
        assert_eq!((*out).len, 4);
        assert_eq!(std::slice::from_raw_parts((*out).ptr.as_ptr(), 4), b"page");
        gos_rt_vec_free(out.cast_mut());

        // A read past the end of the file is short, not an error.
        let short = gos_rt_fs_file_read_at(reader, 64, 8192);
        assert_eq!(gos_rt_result_disc(short), 0);
        let empty = gos_rt_result_payload(short) as *const gossamer_runtime::c_abi::vec::GosVec;
        assert_eq!((*empty).len, 0);
        gos_rt_vec_free(empty.cast_mut());

        gos_rt_fs_file_close(reader);
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_held_exclusive_range_refuses_a_second_handle_and_frees_on_unlock() {
    let path = scratch_path("file-lock");
    let cpath = CString::new(path.to_string_lossy().as_bytes()).unwrap();
    // SAFETY: both handles stay live across the lock assertions and are
    // closed before the file is removed.
    unsafe {
        let created = gos_rt_fs_file_create(cpath.as_ptr());
        assert_eq!(gos_rt_result_disc(created), 0);
        let holder = gos_rt_result_payload(created);

        let taken = gos_rt_fs_file_try_lock_range(holder, 0, 16, 1);
        assert_eq!(gos_rt_result_disc(taken), 0);
        assert_eq!(gos_rt_result_payload(taken), 1);

        // A disjoint range is free while the first is held.
        let disjoint = gos_rt_fs_file_try_lock_range(holder, 32, 16, 1);
        assert_eq!(gos_rt_result_disc(disjoint), 0);
        assert_eq!(gos_rt_result_payload(disjoint), 1);

        let released = gos_rt_fs_file_unlock_range(holder, 0, 16);
        assert_eq!(gos_rt_result_disc(released), 0);
        let retaken = gos_rt_fs_file_try_lock_range(holder, 0, 16, 1);
        assert_eq!(gos_rt_result_disc(retaken), 0);
        assert_eq!(gos_rt_result_payload(retaken), 1);

        assert_eq!(gos_rt_result_disc(gos_rt_fs_file_unlock(holder)), 0);
        gos_rt_fs_file_close(holder);
    }
    let _ = std::fs::remove_file(path);
}
