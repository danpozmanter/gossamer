//! Vec reserve runtime tests.

use gossamer_runtime::c_abi::{
    gos_rt_vec_capacity, gos_rt_vec_free, gos_rt_vec_get_i64, gos_rt_vec_len, gos_rt_vec_new,
    gos_rt_vec_push_i64, gos_rt_vec_reserve_at_least, gos_rt_vec_reserve_exact,
};

#[test]
fn reserve_at_least_preserves_existing_elements_across_inline_split_growth() {
    // SAFETY: The test owns the Vec and releases it after all accesses.
    unsafe {
        let v = gos_rt_vec_new(8);
        assert!(gos_rt_vec_capacity(v) >= 0);
        for i in 0..8 {
            gos_rt_vec_push_i64(v, i);
        }
        gos_rt_vec_reserve_at_least(v, 64);
        assert!(gos_rt_vec_capacity(v) >= 64);
        assert_eq!(gos_rt_vec_len(v), 8);
        for i in 0..8 {
            assert_eq!(gos_rt_vec_get_i64(v, i), i);
        }
        for i in 8..64 {
            gos_rt_vec_push_i64(v, i);
        }
        assert_eq!(gos_rt_vec_len(v), 64);
        for i in 0..64 {
            assert_eq!(gos_rt_vec_get_i64(v, i), i);
        }
        gos_rt_vec_free(v);
    }
}

#[test]
fn reserve_exact_never_shrinks_and_preserves_len() {
    // SAFETY: The test owns the Vec and releases it after all accesses.
    unsafe {
        let v = gos_rt_vec_new(8);
        gos_rt_vec_push_i64(v, 7);
        gos_rt_vec_reserve_exact(v, 1);
        let initial_cap = gos_rt_vec_capacity(v);
        gos_rt_vec_reserve_exact(v, 16);
        assert!(gos_rt_vec_capacity(v) >= 16);
        assert!(gos_rt_vec_capacity(v) >= initial_cap);
        assert_eq!(gos_rt_vec_len(v), 1);
        assert_eq!(gos_rt_vec_get_i64(v, 0), 7);
        gos_rt_vec_free(v);
    }
}
