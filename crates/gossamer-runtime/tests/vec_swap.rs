//! Vec scalar swap runtime tests.

use gossamer_runtime::c_abi::{
    gos_rt_vec_free, gos_rt_vec_get_i64, gos_rt_vec_new, gos_rt_vec_push_i64, gos_rt_vec_swap_i64,
};

#[test]
fn vec_swap_i64_exchanges_in_range_elements_and_ignores_oob() {
    // SAFETY: The test uses a runtime-owned Vec and releases it with
    // `gos_rt_vec_free` after all element accesses.
    unsafe {
        let v = gos_rt_vec_new(8);
        gos_rt_vec_push_i64(v, 10);
        gos_rt_vec_push_i64(v, 20);
        gos_rt_vec_push_i64(v, 30);

        gos_rt_vec_swap_i64(v, 0, 2);
        assert_eq!(gos_rt_vec_get_i64(v, 0), 30);
        assert_eq!(gos_rt_vec_get_i64(v, 1), 20);
        assert_eq!(gos_rt_vec_get_i64(v, 2), 10);

        gos_rt_vec_swap_i64(v, -1, 1);
        gos_rt_vec_swap_i64(v, 1, 99);
        assert_eq!(gos_rt_vec_get_i64(v, 0), 30);
        assert_eq!(gos_rt_vec_get_i64(v, 1), 20);
        assert_eq!(gos_rt_vec_get_i64(v, 2), 10);

        gos_rt_vec_free(v);
    }
}
