//! TCP timeout runtime helper smoke tests.

use gossamer_runtime::c_abi::{
    gos_rt_result_disc, gos_rt_tcp_stream_clear_read_timeout,
    gos_rt_tcp_stream_clear_write_timeout, gos_rt_tcp_stream_set_read_timeout_ms,
    gos_rt_tcp_stream_set_write_timeout_ms,
};

#[test]
fn tcp_timeout_helpers_report_stale_handles_as_errors() {
    // SAFETY: Stale handles are a supported error path and must not deref.
    unsafe {
        assert_eq!(
            gos_rt_result_disc(gos_rt_tcp_stream_set_read_timeout_ms(-99, 10)),
            1
        );
        assert_eq!(
            gos_rt_result_disc(gos_rt_tcp_stream_set_write_timeout_ms(-99, 10)),
            1
        );
        assert_eq!(
            gos_rt_result_disc(gos_rt_tcp_stream_clear_read_timeout(-99)),
            1
        );
        assert_eq!(
            gos_rt_result_disc(gos_rt_tcp_stream_clear_write_timeout(-99)),
            1
        );
    }
}
