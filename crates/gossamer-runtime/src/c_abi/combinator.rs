//! Closure-taking std combinator shims for the compiled tiers:
//! `result::and_then` / `or_else` / `ok` / `err`, the `option::*`
//! family, and the newer closure-taking `iter::*` entries. Mirrors
//! the interp builtins in `gossamer-interp/src/stdlib_builtins` so
//! every tier produces identical results.
//!
//! Closure ABI (shared with the `gos_rt_iter_*_i64` family in
//! `uuid.rs`): `env` is a heap blob whose first word is the callable
//! address — the lifted closure body for capturing closures, or a
//! per-shape thunk (`__fn_thunk_*`) for fn items and non-capturing
//! closures. Callbacks are invoked as `f(env, args…)`.
//!
//! Result / Option ABI: packed `i128` — low word discriminant
//! (0 = Ok/Some, 1 = Err/None), high word payload (see `pack_result`).

use super::{
    GosMap, GosVec, gos_rt_map_insert_i64_i64, gos_rt_map_new, gos_rt_result_disc,
    gos_rt_result_new, gos_rt_result_payload, gos_rt_vec_get_i64, gos_rt_vec_len, gos_rt_vec_new,
    gos_rt_vec_push_i64,
};

type MapFn = unsafe extern "C" fn(env: *const u8, x: i64) -> i64;
type PredFn = unsafe extern "C" fn(env: *const u8, x: i64) -> bool;
type EnumFn = unsafe extern "C" fn(env: *const u8, x: i64) -> i128;
type ThunkEnumFn = unsafe extern "C" fn(env: *const u8) -> i128;
type ThunkValFn = unsafe extern "C" fn(env: *const u8) -> i64;
type CmpFn = unsafe extern "C" fn(env: *const u8, a: i64, b: i64) -> i64;
type VecFn = unsafe extern "C" fn(env: *const u8, x: i64) -> *mut GosVec;

const NONE: i128 = 1;

/// Callable address stored at `env[0]`, or `None` for null/zero envs.
fn env_fn_addr(env: *const u8) -> Option<usize> {
    if env.is_null() {
        return None;
    }
    // SAFETY: `env` is a live closure blob whose first word is the
    // callable address (codegen invariant shared with `gos_rt_iter_*`).
    let addr = unsafe { (env.cast::<usize>()).read() };
    if addr == 0 { None } else { Some(addr) }
}

fn some_of(payload: i64) -> i128 {
    gos_rt_result_new(0, payload)
}

/// Loads the 16-byte enum value whose heap address is `addr` (the
/// payload representation of a nested Result/Option), or None when
/// the address is null.
fn load_enum_at(addr: i64) -> i128 {
    if addr == 0 {
        return NONE;
    }
    // SAFETY: an enum-payload word is the address of the live 16-byte
    // heap copy made at construction (`gos_rt_result_payload_i128`'s
    // contract).
    unsafe {
        let p = addr as usize as *const i64;
        let lo = (*p) as u64 as u128;
        let hi = (*p.add(1)) as u64 as u128;
        ((hi << 64) | lo) as i128
    }
}

/// GC-tracked 2-slot tuple `(a, b)`; the by-value-aggregate ABI
/// memcpys 16 contiguous bytes from the returned pointer.
fn alloc_pair(a: i64, b: i64) -> *mut u8 {
    let p = super::gos_rt_gc_alloc(16);
    if !p.is_null() {
        // SAFETY: `p` is a fresh 16-byte allocation.
        unsafe {
            let slots = p.cast::<i64>();
            *slots = a;
            *slots.add(1) = b;
        }
    }
    p
}

fn vec_elems(v: *const GosVec) -> Vec<i64> {
    if v.is_null() {
        return Vec::new();
    }
    // SAFETY: `v` is a live GosVec; per-index reads go through the
    // bounds-checked accessor.
    let len = unsafe { gos_rt_vec_len(v) };
    (0..len)
        .map(|i| unsafe { gos_rt_vec_get_i64(v, i) })
        .collect()
}

fn vec_from(xs: &[i64]) -> *mut GosVec {
    // SAFETY: fresh vec; push copies values in.
    let out = unsafe { gos_rt_vec_new(8) };
    for &x in xs {
        unsafe { gos_rt_vec_push_i64(out, x) };
    }
    out
}

// ----------------------------------------------------------------------
// result

/// `result::and_then(f, res)` — `f(ok_payload) -> Result` when Ok,
/// Err passthrough. Callback returns a packed i128 Result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_and_then(res: i128, env: *const u8) -> i128 {
    ffi_entry!(0i128, {
        if gos_rt_result_disc(res) != 0 {
            return res;
        }
        let Some(addr) = env_fn_addr(env) else {
            return res;
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: EnumFn = unsafe { std::mem::transmute(addr) };
        unsafe { f(env, gos_rt_result_payload(res)) }
    })
}

/// `result::or_else(f, res)` — `f(err_payload) -> Result` when Err,
/// Ok passthrough.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_or_else(res: i128, env: *const u8) -> i128 {
    ffi_entry!(0i128, {
        if gos_rt_result_disc(res) == 0 {
            return res;
        }
        let Some(addr) = env_fn_addr(env) else {
            return res;
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: EnumFn = unsafe { std::mem::transmute(addr) };
        unsafe { f(env, gos_rt_result_payload(res)) }
    })
}

/// `result::ok(res) -> Option<T>` — Ok payload as Some, Err as None.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_to_opt_ok(res: i128) -> i128 {
    if gos_rt_result_disc(res) == 0 {
        some_of(gos_rt_result_payload(res))
    } else {
        NONE
    }
}

/// `result::err(res) -> Option<E>` — Err payload as Some, Ok as None.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_to_opt_err(res: i128) -> i128 {
    if gos_rt_result_disc(res) != 0 {
        some_of(gos_rt_result_payload(res))
    } else {
        NONE
    }
}

// ----------------------------------------------------------------------
// option

/// `option::and_then(f, opt)` — `f(payload) -> Option` when Some,
/// None passthrough.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_option_and_then(opt: i128, env: *const u8) -> i128 {
    ffi_entry!(NONE, {
        if gos_rt_result_disc(opt) != 0 {
            return NONE;
        }
        let Some(addr) = env_fn_addr(env) else {
            return NONE;
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: EnumFn = unsafe { std::mem::transmute(addr) };
        unsafe { f(env, gos_rt_result_payload(opt)) }
    })
}

/// `option::filter(p, opt)` — keeps Some(x) only when `p(x)` holds.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_option_filter(opt: i128, env: *const u8) -> i128 {
    ffi_entry!(NONE, {
        if gos_rt_result_disc(opt) != 0 {
            return NONE;
        }
        let Some(addr) = env_fn_addr(env) else {
            return NONE;
        };
        let payload = gos_rt_result_payload(opt);
        // SAFETY: addr is the callable stored by the closure lowering.
        let p: PredFn = unsafe { std::mem::transmute(addr) };
        if unsafe { p(env, payload) } {
            some_of(payload)
        } else {
            NONE
        }
    })
}

/// `option::or(alt, opt)` — data-last: returns `opt` when Some,
/// otherwise `alt`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_option_or(alt: i128, opt: i128) -> i128 {
    if gos_rt_result_disc(opt) == 0 {
        opt
    } else {
        alt
    }
}

/// `option::or_else(f, opt)` — returns `opt` when Some, otherwise
/// `f() -> Option`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_option_or_else(opt: i128, env: *const u8) -> i128 {
    ffi_entry!(NONE, {
        if gos_rt_result_disc(opt) == 0 {
            return opt;
        }
        let Some(addr) = env_fn_addr(env) else {
            return NONE;
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: ThunkEnumFn = unsafe { std::mem::transmute(addr) };
        unsafe { f(env) }
    })
}

/// `option::default_with(f, opt)` — Some payload, or `f()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_option_default_with(opt: i128, env: *const u8) -> i64 {
    ffi_entry!(0, {
        if gos_rt_result_disc(opt) == 0 {
            return gos_rt_result_payload(opt);
        }
        let Some(addr) = env_fn_addr(env) else {
            return 0;
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: ThunkValFn = unsafe { std::mem::transmute(addr) };
        unsafe { f(env) }
    })
}

/// `option::zip(first, second)` — `Some((a, b))` when both are Some.
/// Matches the interp's argument order: the data-last pipe passes the
/// piped option as `second`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_option_zip(first: i128, second: i128) -> i128 {
    ffi_entry!(NONE, {
        if gos_rt_result_disc(first) != 0 || gos_rt_result_disc(second) != 0 {
            return NONE;
        }
        let pair = alloc_pair(gos_rt_result_payload(first), gos_rt_result_payload(second));
        if pair.is_null() {
            return NONE;
        }
        some_of(pair as i64)
    })
}

/// `option::flatten(opt)` — `Some(inner)` loads the nested 16-byte
/// Option from the payload word; None stays None.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_option_flatten(opt: i128) -> i128 {
    if gos_rt_result_disc(opt) != 0 {
        return NONE;
    }
    load_enum_at(gos_rt_result_payload(opt))
}

/// `option::iter(opt) -> [T]` — zero- or one-element Vec.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_option_iter(opt: i128) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        if gos_rt_result_disc(opt) == 0 {
            vec_from(&[gos_rt_result_payload(opt)])
        } else {
            vec_from(&[])
        }
    })
}

// ----------------------------------------------------------------------
// iter — closure-taking entries over Vec<i64>-shaped sequences.

/// `iter::filter_map(f, xs)` — keeps the Some payloads of `f(x)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_filter_map_i64(
    env: *const u8,
    v: *const GosVec,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let Some(addr) = env_fn_addr(env) else {
            return vec_from(&[]);
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: EnumFn = unsafe { std::mem::transmute(addr) };
        let mut out = Vec::new();
        for x in vec_elems(v) {
            let r = unsafe { f(env, x) };
            if gos_rt_result_disc(r) == 0 {
                out.push(gos_rt_result_payload(r));
            }
        }
        vec_from(&out)
    })
}

/// `iter::flat_map(f, xs)` — concatenates the Vec results of `f(x)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_flat_map_i64(env: *const u8, v: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let Some(addr) = env_fn_addr(env) else {
            return vec_from(&[]);
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: VecFn = unsafe { std::mem::transmute(addr) };
        let mut out = Vec::new();
        for x in vec_elems(v) {
            let inner = unsafe { f(env, x) };
            out.extend(vec_elems(inner));
        }
        vec_from(&out)
    })
}

/// `iter::flat_map(f, xs)` variant for callbacks returning a
/// fixed-size array: the result pointer is a raw buffer of
/// `arr_len` i64 slots (no GosVec header).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_flat_map_arr_i64(
    env: *const u8,
    v: *const GosVec,
    arr_len: i64,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let Some(addr) = env_fn_addr(env) else {
            return vec_from(&[]);
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: MapFn = unsafe { std::mem::transmute(addr) };
        let mut out = Vec::new();
        for x in vec_elems(v) {
            let buf = unsafe { f(env, x) } as usize as *const i64;
            if buf.is_null() {
                continue;
            }
            for i in 0..arr_len.max(0) {
                // SAFETY: the callback returned a live buffer of
                // `arr_len` contiguous i64 slots (fixed-array ABI).
                out.push(unsafe { buf.add(i as usize).read() });
            }
        }
        vec_from(&out)
    })
}

/// `iter::reduce(f, xs) -> Option<T>` — left fold seeded by the first
/// element; None on an empty sequence.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_reduce_i64(env: *const u8, v: *const GosVec) -> i128 {
    ffi_entry!(NONE, {
        let xs = vec_elems(v);
        let Some((&first, rest)) = xs.split_first() else {
            return NONE;
        };
        let Some(addr) = env_fn_addr(env) else {
            return NONE;
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: CmpFn = unsafe { std::mem::transmute(addr) };
        let mut acc = first;
        for &x in rest {
            acc = unsafe { f(env, acc, x) };
        }
        some_of(acc)
    })
}

/// `iter::scan(init, f, xs)` — running fold, one output per element.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_scan_i64(
    init: i64,
    env: *const u8,
    v: *const GosVec,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let Some(addr) = env_fn_addr(env) else {
            return vec_from(&[]);
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: CmpFn = unsafe { std::mem::transmute(addr) };
        let mut acc = init;
        let mut out = Vec::new();
        for x in vec_elems(v) {
            acc = unsafe { f(env, acc, x) };
            out.push(acc);
        }
        vec_from(&out)
    })
}

/// `iter::product_by(f, xs)` — product of `f(x)` over the sequence.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_product_by_i64(env: *const u8, v: *const GosVec) -> i64 {
    ffi_entry!(1, {
        let Some(addr) = env_fn_addr(env) else {
            return 1;
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: MapFn = unsafe { std::mem::transmute(addr) };
        let mut prod: i64 = 1;
        for x in vec_elems(v) {
            prod = prod.wrapping_mul(unsafe { f(env, x) });
        }
        prod
    })
}

/// `iter::position(p, xs) -> Option<i64>` — index of the first match.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_position_i64(env: *const u8, v: *const GosVec) -> i128 {
    ffi_entry!(NONE, {
        let Some(addr) = env_fn_addr(env) else {
            return NONE;
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let p: PredFn = unsafe { std::mem::transmute(addr) };
        for (i, x) in vec_elems(v).into_iter().enumerate() {
            if unsafe { p(env, x) } {
                return some_of(i as i64);
            }
        }
        NONE
    })
}

/// `iter::find_map(f, xs) -> Option<U>` — first Some payload of `f(x)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_find_map_i64(env: *const u8, v: *const GosVec) -> i128 {
    ffi_entry!(NONE, {
        let Some(addr) = env_fn_addr(env) else {
            return NONE;
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: EnumFn = unsafe { std::mem::transmute(addr) };
        for x in vec_elems(v) {
            let r = unsafe { f(env, x) };
            if gos_rt_result_disc(r) == 0 {
                return some_of(gos_rt_result_payload(r));
            }
        }
        NONE
    })
}

/// `iter::take_while(p, xs)` — longest matching prefix.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_take_while_i64(
    env: *const u8,
    v: *const GosVec,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let Some(addr) = env_fn_addr(env) else {
            return vec_from(&[]);
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let p: PredFn = unsafe { std::mem::transmute(addr) };
        let mut out = Vec::new();
        for x in vec_elems(v) {
            if unsafe { p(env, x) } {
                out.push(x);
            } else {
                break;
            }
        }
        vec_from(&out)
    })
}

/// `iter::skip_while(p, xs)` — everything after the matching prefix.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_skip_while_i64(
    env: *const u8,
    v: *const GosVec,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let Some(addr) = env_fn_addr(env) else {
            return vec_from(&[]);
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let p: PredFn = unsafe { std::mem::transmute(addr) };
        let mut out = Vec::new();
        let mut dropping = true;
        for x in vec_elems(v) {
            if dropping && unsafe { p(env, x) } {
                continue;
            }
            dropping = false;
            out.push(x);
        }
        vec_from(&out)
    })
}

/// `iter::partition(p, xs) -> ([T], [T])` — matching elements first.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_partition_i64(env: *const u8, v: *const GosVec) -> *mut u8 {
    ffi_entry!(std::ptr::null_mut(), {
        let Some(addr) = env_fn_addr(env) else {
            return alloc_pair(vec_from(&[]) as i64, vec_from(&[]) as i64);
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let p: PredFn = unsafe { std::mem::transmute(addr) };
        let mut yes = Vec::new();
        let mut no = Vec::new();
        for x in vec_elems(v) {
            if unsafe { p(env, x) } {
                yes.push(x);
            } else {
                no.push(x);
            }
        }
        alloc_pair(vec_from(&yes) as i64, vec_from(&no) as i64)
    })
}

/// Comparator-driven stable sort shared by `sorted_by` and the keyed
/// orderings. `cmp` maps the callback result to an `Ordering` the same
/// way the interp does (`signum`).
fn sorted_with(env: *const u8, v: *const GosVec, f: CmpFn) -> Vec<i64> {
    let mut xs = vec_elems(v);
    // SAFETY: the callback contract is upheld by the closure lowering.
    xs.sort_by(|&a, &b| unsafe { f(env, a, b) }.cmp(&0));
    xs
}

/// `iter::sort_by(cmp, xs)` — fresh sorted Vec (non-mutating).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_sorted_by_i64(
    env: *const u8,
    v: *const GosVec,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let Some(addr) = env_fn_addr(env) else {
            return vec_from(&vec_elems(v));
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: CmpFn = unsafe { std::mem::transmute(addr) };
        vec_from(&sorted_with(env, v, f))
    })
}

/// `iter::sort_by_key(key, xs)` — fresh Vec sorted by `key(x)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_sorted_by_key_i64(
    env: *const u8,
    v: *const GosVec,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let Some(addr) = env_fn_addr(env) else {
            return vec_from(&vec_elems(v));
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: MapFn = unsafe { std::mem::transmute(addr) };
        let mut keyed: Vec<(i64, i64)> = vec_elems(v)
            .into_iter()
            .map(|x| (unsafe { f(env, x) }, x))
            .collect();
        keyed.sort_by_key(|&(k, _)| k);
        let out: Vec<i64> = keyed.into_iter().map(|(_, x)| x).collect();
        vec_from(&out)
    })
}

/// `iter::min_by(cmp, xs) -> Option<T>` — first minimal element wins
/// ties (matches the interp's `cmp(x, best) < 0` update rule).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_min_by_i64(env: *const u8, v: *const GosVec) -> i128 {
    ffi_entry!(NONE, {
        let xs = vec_elems(v);
        let Some((&first, rest)) = xs.split_first() else {
            return NONE;
        };
        let Some(addr) = env_fn_addr(env) else {
            return some_of(first);
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: CmpFn = unsafe { std::mem::transmute(addr) };
        let mut best = first;
        for &x in rest {
            if unsafe { f(env, x, best) } < 0 {
                best = x;
            }
        }
        some_of(best)
    })
}

/// `iter::max_by(cmp, xs) -> Option<T>` — first maximal element wins
/// ties (`cmp(x, best) > 0` update rule).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_max_by_i64(env: *const u8, v: *const GosVec) -> i128 {
    ffi_entry!(NONE, {
        let xs = vec_elems(v);
        let Some((&first, rest)) = xs.split_first() else {
            return NONE;
        };
        let Some(addr) = env_fn_addr(env) else {
            return some_of(first);
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: CmpFn = unsafe { std::mem::transmute(addr) };
        let mut best = first;
        for &x in rest {
            if unsafe { f(env, x, best) } > 0 {
                best = x;
            }
        }
        some_of(best)
    })
}

/// `iter::min_by_key(key, xs) -> Option<T>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_min_by_key_i64(env: *const u8, v: *const GosVec) -> i128 {
    ffi_entry!(NONE, { min_max_by_key(env, v, false) })
}

/// `iter::max_by_key(key, xs) -> Option<T>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_max_by_key_i64(env: *const u8, v: *const GosVec) -> i128 {
    ffi_entry!(NONE, { min_max_by_key(env, v, true) })
}

fn min_max_by_key(env: *const u8, v: *const GosVec, want_max: bool) -> i128 {
    let xs = vec_elems(v);
    let Some((&first, rest)) = xs.split_first() else {
        return NONE;
    };
    let Some(addr) = env_fn_addr(env) else {
        return some_of(first);
    };
    // SAFETY: addr is the callable stored by the closure lowering.
    let f: MapFn = unsafe { std::mem::transmute(addr) };
    let mut best = first;
    let mut best_key = unsafe { f(env, first) };
    for &x in rest {
        let k = unsafe { f(env, x) };
        let better = if want_max { k > best_key } else { k < best_key };
        if better {
            best = x;
            best_key = k;
        }
    }
    some_of(best)
}

/// `iter::group_by(key, xs) -> HashMap<K, [T]>` — insertion order of
/// groups follows first occurrence of each key.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_group_by_i64(env: *const u8, v: *const GosVec) -> *mut GosMap {
    ffi_entry!(std::ptr::null_mut(), {
        // SAFETY: fresh map; inserts go through the i64-keyed API.
        let map = unsafe { gos_rt_map_new(8, 8) };
        let Some(addr) = env_fn_addr(env) else {
            return map;
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: MapFn = unsafe { std::mem::transmute(addr) };
        let mut groups: Vec<(i64, Vec<i64>)> = Vec::new();
        for x in vec_elems(v) {
            let k = unsafe { f(env, x) };
            match groups.iter_mut().find(|(key, _)| *key == k) {
                Some((_, members)) => members.push(x),
                None => groups.push((k, vec![x])),
            }
        }
        for (k, members) in groups {
            unsafe { gos_rt_map_insert_i64_i64(map, k, vec_from(&members) as i64) };
        }
        map
    })
}

/// `iter::count_by(key, xs) -> HashMap<K, i64>` — occurrence counts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_iter_count_by_i64(env: *const u8, v: *const GosVec) -> *mut GosMap {
    ffi_entry!(std::ptr::null_mut(), {
        // SAFETY: fresh map; inserts go through the i64-keyed API.
        let map = unsafe { gos_rt_map_new(8, 8) };
        let Some(addr) = env_fn_addr(env) else {
            return map;
        };
        // SAFETY: addr is the callable stored by the closure lowering.
        let f: MapFn = unsafe { std::mem::transmute(addr) };
        let mut counts: Vec<(i64, i64)> = Vec::new();
        for x in vec_elems(v) {
            let k = unsafe { f(env, x) };
            match counts.iter_mut().find(|(key, _)| *key == k) {
                Some((_, n)) => *n += 1,
                None => counts.push((k, 1)),
            }
        }
        for (k, n) in counts {
            unsafe { gos_rt_map_insert_i64_i64(map, k, n) };
        }
        map
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Heap-copies a 16-byte enum value and returns its address —
    /// the payload representation of a nested Result/Option.
    fn store_enum(value: i128) -> i64 {
        let p = crate::c_abi::gos_rt_gc_alloc(16);
        assert!(!p.is_null());
        // SAFETY: `p` is a fresh 16-byte allocation.
        unsafe {
            let slots = p.cast::<i64>();
            *slots = value as u64 as i64;
            *slots.add(1) = ((value as u128) >> 64) as u64 as i64;
        }
        p as i64
    }

    extern "C" fn double_cb(_env: *const u8, x: i64) -> i64 {
        x * 2
    }
    extern "C" fn even_cb(_env: *const u8, x: i64) -> bool {
        x % 2 == 0
    }
    extern "C" fn sub_cb(_env: *const u8, a: i64, b: i64) -> i64 {
        a - b
    }
    extern "C" fn opt_pos_cb(_env: *const u8, x: i64) -> i128 {
        if x > 0 {
            gos_rt_result_new(0, x * 10)
        } else {
            NONE
        }
    }

    fn env_for(addr: usize) -> Vec<usize> {
        vec![addr, 0]
    }

    fn elems(v: *mut GosVec) -> Vec<i64> {
        vec_elems(v)
    }

    #[test]
    fn option_filter_keeps_matching_payload() {
        let env = env_for(even_cb as *const () as usize);
        let some4 = gos_rt_result_new(0, 4);
        let some3 = gos_rt_result_new(0, 3);
        let kept = unsafe { gos_rt_option_filter(some4, env.as_ptr().cast()) };
        let dropped = unsafe { gos_rt_option_filter(some3, env.as_ptr().cast()) };
        assert_eq!(gos_rt_result_disc(kept), 0);
        assert_eq!(gos_rt_result_payload(kept), 4);
        assert_eq!(gos_rt_result_disc(dropped), 1);
    }

    #[test]
    fn option_zip_pairs_payloads_in_arg_order() {
        let a = gos_rt_result_new(0, 1);
        let b = gos_rt_result_new(0, 2);
        let zipped = gos_rt_option_zip(b, a);
        assert_eq!(gos_rt_result_disc(zipped), 0);
        let pair = gos_rt_result_payload(zipped) as usize as *const i64;
        // SAFETY: zip allocated a live 2-slot pair.
        unsafe {
            assert_eq!(*pair, 2);
            assert_eq!(*pair.add(1), 1);
        }
    }

    #[test]
    fn option_flatten_loads_nested_enum() {
        let inner = gos_rt_result_new(0, 42);
        let outer = gos_rt_result_new(0, store_enum(inner));
        let flat = gos_rt_option_flatten(outer);
        assert_eq!(gos_rt_result_disc(flat), 0);
        assert_eq!(gos_rt_result_payload(flat), 42);
        assert_eq!(gos_rt_result_disc(gos_rt_option_flatten(NONE)), 1);
    }

    #[test]
    fn result_and_then_chains_ok_payload() {
        let env = env_for(opt_pos_cb as *const () as usize);
        let ok = gos_rt_result_new(0, 5);
        let out = unsafe { gos_rt_result_and_then(ok, env.as_ptr().cast()) };
        assert_eq!(gos_rt_result_disc(out), 0);
        assert_eq!(gos_rt_result_payload(out), 50);
        let err = gos_rt_result_new(1, 9);
        let passthrough = unsafe { gos_rt_result_and_then(err, env.as_ptr().cast()) };
        assert_eq!(gos_rt_result_disc(passthrough), 1);
        assert_eq!(gos_rt_result_payload(passthrough), 9);
    }

    #[test]
    fn iter_partition_splits_matching_first() {
        let env = env_for(even_cb as *const () as usize);
        let v = vec_from(&[1, 2, 3, 4]);
        let pair = unsafe { gos_rt_iter_partition_i64(env.as_ptr().cast(), v) };
        // SAFETY: partition returns a live 2-slot pair of vec pointers.
        let (yes, no) = unsafe {
            let slots = pair.cast::<i64>();
            (
                *slots as usize as *mut GosVec,
                *slots.add(1) as usize as *mut GosVec,
            )
        };
        assert_eq!(elems(yes), vec![2, 4]);
        assert_eq!(elems(no), vec![1, 3]);
    }

    #[test]
    fn iter_scan_reduce_sorted_follow_interp_semantics() {
        let cmp_env = env_for(sub_cb as *const () as usize);
        let scanned =
            unsafe { gos_rt_iter_scan_i64(0, cmp_env.as_ptr().cast(), vec_from(&[1, 2, 3])) };
        assert_eq!(elems(scanned), vec![-1, -3, -6]);
        let reduced =
            unsafe { gos_rt_iter_reduce_i64(cmp_env.as_ptr().cast(), vec_from(&[10, 1, 2])) };
        assert_eq!(gos_rt_result_payload(reduced), 7);
        let sorted =
            unsafe { gos_rt_iter_sorted_by_i64(cmp_env.as_ptr().cast(), vec_from(&[3, 1, 2])) };
        assert_eq!(elems(sorted), vec![1, 2, 3]);
        let map_env = env_for(double_cb as *const () as usize);
        let keyed =
            unsafe { gos_rt_iter_sorted_by_key_i64(map_env.as_ptr().cast(), vec_from(&[3, 1, 2])) };
        assert_eq!(elems(keyed), vec![1, 2, 3]);
    }
}
