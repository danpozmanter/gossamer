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

use crate::c_abi::GosVec;
use indexmap::{IndexMap, IndexSet};
use rustc_hash::FxBuildHasher;
use std::os::raw::c_char;

/// A membership table keyed by content, in the order elements were added.
type SetTable<T> = IndexSet<T, FxBuildHasher>;
/// The aggregate family's canonical-bytes to stored-slots table, in the order
/// elements were added.
type AggregateTable = IndexMap<Box<[u8]>, Box<[u8]>, FxBuildHasher>;

// ---------------------------------------------------------------
// Sets - one heap table per element family, with the pointer to the
// table being the value user code sees. Each family stores its
// elements in their own representation: text keys as `String`,
// integer keys as `i64`, and struct/tuple keys by their canonical
// slot bytes.
//
// A `Set` traverses in no particular order: the tables keep the order
// elements were added, which every tier reproduces, and no program may
// rely on it. A `BTreeSet` sorts, and carries `ordered` to say so.
// ---------------------------------------------------------------

#[derive(Clone, Default)]
pub struct GosSet {
    inner: SetTable<String>,
    /// Integer elements keep their numeric representation: a decimal-text
    /// encoding would cost a formatting pass and an allocation per membership
    /// test, and store roughly twice the bytes per live element.
    i64_inner: SetTable<i64>,
    /// Aggregate elements are keyed by their canonical slot bytes and retain
    /// an owned copy of those slots for `iter()` / set algebra.
    struct_inner: AggregateTable,
    /// A `BTreeSet` reads its elements in sorted order; a `Set` reads them in
    /// the table's own order. Set algebra carries the flag onto its result, so
    /// `a.union(b)` reads the way `a` does.
    ordered: bool,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_new() -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosSet::default()))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btree_set_new() -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosSet {
            ordered: true,
            ..GosSet::default()
        }))
    })
}

/// Marks a set as reading in sorted order (the `BTreeSet` contract).
fn mark_ordered(set: *mut GosSet) -> *mut GosSet {
    if !set.is_null() {
        unsafe { &mut *set }.ordered = true;
    }
    set
}

/// `xs.clone()` for a `Set` / `BTreeSet` receiver, and the primitive a
/// `let` binding or by-value call argument uses to give the binding an
/// independent table instead of aliasing the source. Every element `GosSet`
/// stores is owned content (a `String`, or a struct's canonical slot bytes)
/// rather than an RC pointer, so a structural `Clone` deep-copies the whole
/// table with no child retain pass needed - unlike `gos_rt_map_clone`.
///
/// `GosSet` carries no refcount of its own (unlike `GosVec` / strings) - a
/// plain pointer copy at a `let` binding either double-frees the table once
/// both bindings' drop points run, or leaves both bindings mutating the
/// same live table.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_clone(src: *const GosSet) -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        if src.is_null() {
            return unsafe { gos_rt_set_new() };
        }
        Box::into_raw(Box::new(unsafe { &*src }.clone()))
    })
}

/// Builds a set from a `GosVec` of scalar slots. `Set::from(values)` where
/// `values` is a runtime sequence rather than a literal list; the literal
/// form is unrolled into individual inserts at lowering time.
///
/// # Safety
/// `v` must be null or a live `GosVec` whose slots hold `i64` elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_from_vec_i64(v: *const GosVec) -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        let set = unsafe { gos_rt_set_new() };
        if v.is_null() || set.is_null() {
            return set;
        }
        let vec = unsafe { &*v };
        let ptr = vec.ptr.cast::<i64>();
        let out = unsafe { &mut *set };
        for i in 0..vec.len.max(0) as usize {
            out.i64_inner.insert(unsafe { *ptr.add(i) });
        }
        set
    })
}

/// Builds a set from a `GosVec` of string slots.
///
/// # Safety
/// `v` must be null or a live `GosVec` whose slots hold `*const c_char`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_from_vec_str(v: *const GosVec) -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        let set = unsafe { gos_rt_set_new() };
        if v.is_null() || set.is_null() {
            return set;
        }
        let vec = unsafe { &*v };
        let ptr = vec.ptr.cast::<*const c_char>();
        let out = unsafe { &mut *set };
        for i in 0..vec.len.max(0) as usize {
            let entry = unsafe { *ptr.add(i) };
            if entry.is_null() {
                continue;
            }
            out.inner
                .insert(unsafe { crate::c_abi::gos_str_arg_string(entry) });
        }
        set
    })
}

/// `BTreeSet::from(values)`; the ordered set shares this representation and
/// sorts on iteration.
///
/// # Safety
/// Same contract as [`gos_rt_set_from_vec_i64`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btree_set_from_vec_i64(v: *const GosVec) -> *mut GosSet {
    mark_ordered(unsafe { gos_rt_set_from_vec_i64(v) })
}

/// # Safety
/// Same contract as [`gos_rt_set_from_vec_str`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btree_set_from_vec_str(v: *const GosVec) -> *mut GosSet {
    mark_ordered(unsafe { gos_rt_set_from_vec_str(v) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_insert(s: *mut GosSet, key: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() || key.is_null() {
            return 0;
        }
        let k = unsafe { crate::c_abi::gos_str_arg_string(key) };
        let s = unsafe { &mut *s };
        i64::from(s.inner.insert(k))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_contains(s: *const GosSet, key: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() || key.is_null() {
            return 0;
        }
        let bytes = unsafe { crate::c_abi::gos_str_arg_bytes(key) };
        // Gossamer strings are always valid UTF-8 at the source level.
        let k: &str = unsafe { std::str::from_utf8_unchecked(bytes) };
        let s = unsafe { &*s };
        i64::from(s.inner.contains(k))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_remove(s: *mut GosSet, key: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() || key.is_null() {
            return 0;
        }
        let bytes = unsafe { crate::c_abi::gos_str_arg_bytes(key) };
        // Gossamer strings are always valid UTF-8 at the source level.
        let k: &str = unsafe { std::str::from_utf8_unchecked(bytes) };
        let s = unsafe { &mut *s };
        i64::from(s.inner.shift_remove(k))
    })
}

// `HashSet<i64>` keeps its own numeric table. The MIR dispatch routes
// i64-element sets to these entry points and routes `to_vec` to the i64
// reader, which sorts numerically to match the VM's `MapKey::Int` ordering.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_insert_i64(s: *mut GosSet, key: i64) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() {
            return 0;
        }
        let s = unsafe { &mut *s };
        i64::from(s.i64_inner.insert(key))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_contains_i64(s: *const GosSet, key: i64) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() {
            return 0;
        }
        let s = unsafe { &*s };
        i64::from(s.i64_inner.contains(&key))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_remove_i64(s: *mut GosSet, key: i64) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() {
            return 0;
        }
        let s = unsafe { &mut *s };
        i64::from(s.i64_inner.shift_remove(&key))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_len(s: *const GosSet) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() {
            return 0;
        }
        let s = unsafe { &*s };
        (s.inner.len() + s.i64_inner.len() + s.struct_inner.len()) as i64
    })
}

fn set_format_prefix(ordered: i32) -> &'static str {
    if ordered != 0 { "BTreeSet" } else { "Set" }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_format_i64(s: *const GosSet, ordered: i32) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let mut out = String::from(set_format_prefix(ordered));
        out.push_str(" {");
        if !s.is_null() {
            let set = unsafe { &*s };
            let mut keys: Vec<i64> = set.i64_inner.iter().copied().collect();
            keys.sort_unstable();
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push_str(&crate::builtins::format_int(*key));
            }
        }
        out.push('}');
        crate::c_abi::string::alloc_cstring(out.as_bytes())
    })
}

/// Renders a set whose elements are aggregates, each stored as its canonical
/// slot bytes and rendered through the descriptor `tags` addresses.
///
/// # Safety
/// `s` is a live `GosSet` and `tags` addresses a descriptor for its elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_format_desc(
    s: *const GosSet,
    ordered: i32,
    tags: *const u8,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let mut out = String::from(set_format_prefix(ordered));
        out.push_str(" {");
        if !s.is_null() && !tags.is_null() {
            let set = unsafe { &*s };
            let mut entries: Vec<&Box<[u8]>> = set.struct_inner.values().collect();
            entries.sort_unstable();
            for (index, slots) in entries.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                let mut cursor = 0usize;
                unsafe {
                    crate::c_abi::map::render_desc_value(
                        &mut out,
                        slots.as_ptr(),
                        tags,
                        &mut cursor,
                    );
                }
            }
        }
        out.push('}');
        crate::c_abi::string::alloc_cstring(out.as_bytes())
    })
}

/// Renders an integer set whose elements were declared `u64` / `usize`: the
/// same slots as [`gos_rt_set_format_i64`], read as unsigned so an element at
/// or above `i64::MAX` shows its own decimal.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_format_u64(s: *const GosSet, ordered: i32) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let mut out = String::from(set_format_prefix(ordered));
        out.push_str(" {");
        if !s.is_null() {
            let set = unsafe { &*s };
            let mut keys: Vec<u64> = set.i64_inner.iter().map(|n| *n as u64).collect();
            keys.sort_unstable();
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push_str(&crate::builtins::format_uint(*key));
            }
        }
        out.push('}');
        crate::c_abi::string::alloc_cstring(out.as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_format_string(s: *const GosSet, ordered: i32) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let mut out = String::from(set_format_prefix(ordered));
        out.push_str(" {");
        if !s.is_null() {
            let set = unsafe { &*s };
            let mut keys: Vec<&str> = set.inner.iter().map(String::as_str).collect();
            keys.sort_unstable();
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push_str(key);
            }
        }
        out.push('}');
        crate::c_abi::string::alloc_cstring(out.as_bytes())
    })
}

/// Snapshots a string set's keys into a fresh `Vec<String>`: sorted for a
/// `BTreeSet`, and otherwise in the table's own order, which a `Set` gives no
/// meaning to beyond being the same on every tier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_to_vec(s: *const GosSet) -> *mut crate::c_abi::vec::GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe {
            crate::c_abi::vec::gos_rt_vec_new_typed(8, crate::c_abi::vec::vec_elem_kind::STRING)
        };
        if s.is_null() {
            return out;
        }
        let s = unsafe { &*s };
        let mut keys: Vec<&str> = s.inner.iter().map(String::as_str).collect();
        if s.ordered {
            keys.sort_unstable();
        }
        for k in keys {
            let cstr = crate::c_abi::string::alloc_cstring(k.as_bytes());
            let slot = (cstr as usize as i64).to_ne_bytes();
            unsafe { crate::c_abi::vec::gos_rt_vec_push(out, slot.as_ptr()) };
        }
        out
    })
}

/// Snapshots an i64 set's keys into a fresh `Vec<i64>`: sorted numerically for
/// a `BTreeSet`, and otherwise in the table's own order.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_to_vec_i64(s: *const GosSet) -> *mut crate::c_abi::vec::GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe {
            crate::c_abi::vec::gos_rt_vec_new_typed(8, crate::c_abi::vec::vec_elem_kind::PRIMITIVE)
        };
        if s.is_null() {
            return out;
        }
        let s = unsafe { &*s };
        let mut keys: Vec<i64> = s.i64_inner.iter().copied().collect();
        if s.ordered {
            keys.sort_unstable();
        }
        for k in keys {
            unsafe { crate::c_abi::vec::gos_rt_vec_push_i64(out, k) };
        }
        out
    })
}

/// Snapshots the intersection of two string sets without first allocating a
/// temporary `HashSet`. This is the native lowering for
/// `left.intersection(right).iter()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_intersection_to_vec(
    a: *const GosSet,
    b: *const GosSet,
) -> *mut crate::c_abi::vec::GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe {
            crate::c_abi::vec::gos_rt_vec_new_typed(8, crate::c_abi::vec::vec_elem_kind::STRING)
        };
        let (a, b) = unsafe { set_refs(a, b) };
        let mut keys: Vec<&str> = a.inner.intersection(&b.inner).map(String::as_str).collect();
        if a.ordered {
            keys.sort_unstable();
        }
        for key in keys {
            let cstr = crate::c_abi::string::alloc_cstring(key.as_bytes());
            let slot = (cstr as usize as i64).to_ne_bytes();
            unsafe { crate::c_abi::vec::gos_rt_vec_push(out, slot.as_ptr()) };
        }
        out
    })
}

/// i64 counterpart of [`gos_rt_set_intersection_to_vec`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_intersection_to_vec_i64(
    a: *const GosSet,
    b: *const GosSet,
) -> *mut crate::c_abi::vec::GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let out = unsafe {
            crate::c_abi::vec::gos_rt_vec_new_typed(8, crate::c_abi::vec::vec_elem_kind::PRIMITIVE)
        };
        let (a, b) = unsafe { set_refs(a, b) };
        let mut keys: Vec<i64> = a.i64_inner.intersection(&b.i64_inner).copied().collect();
        if a.ordered {
            keys.sort_unstable();
        }
        for key in keys {
            unsafe { crate::c_abi::vec::gos_rt_vec_push_i64(out, key) };
        }
        out
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_clear(s: *mut GosSet) -> *mut GosSet {
    ffi_entry!(s, {
        if !s.is_null() {
            let s = unsafe { &mut *s };
            s.inner.clear();
            s.i64_inner.clear();
            s.struct_inner.clear();
        }
        s
    })
}

/// Borrows the two operand sets, or returns empty borrows for null
/// pointers so the algebra shims never deref a null handle.
unsafe fn set_refs<'a>(a: *const GosSet, b: *const GosSet) -> (&'a GosSet, &'a GosSet) {
    static EMPTY: std::sync::OnceLock<GosSet> = std::sync::OnceLock::new();
    let empty = EMPTY.get_or_init(GosSet::default);
    let a = if a.is_null() { empty } else { unsafe { &*a } };
    let b = if b.is_null() { empty } else { unsafe { &*b } };
    (a, b)
}

/// Combines two sets element-family by element-family. Each family is
/// independent, so a set only ever populates one of them and the other
/// combinations reduce to empty tables.
unsafe fn set_combine(
    a: *const GosSet,
    b: *const GosSet,
    text: impl Fn(&SetTable<String>, &SetTable<String>) -> SetTable<String>,
    ints: impl Fn(&SetTable<i64>, &SetTable<i64>) -> SetTable<i64>,
    aggregates: impl Fn(&AggregateTable, &AggregateTable) -> AggregateTable,
) -> *mut GosSet {
    let (a, b) = unsafe { set_refs(a, b) };
    Box::into_raw(Box::new(GosSet {
        inner: text(&a.inner, &b.inner),
        i64_inner: ints(&a.i64_inner, &b.i64_inner),
        struct_inner: aggregates(&a.struct_inner, &b.struct_inner),
        // The result is read the way the receiver is: `a.union(b)` on a
        // `BTreeSet` answers a `BTreeSet`.
        ordered: a.ordered,
    }))
}

/// True when `pred` holds for every element family of the two operands.
unsafe fn set_relation(
    a: *const GosSet,
    b: *const GosSet,
    text: impl Fn(&SetTable<String>, &SetTable<String>) -> bool,
    ints: impl Fn(&SetTable<i64>, &SetTable<i64>) -> bool,
    aggregates: impl Fn(&AggregateTable, &AggregateTable) -> bool,
) -> i64 {
    let (a, b) = unsafe { set_refs(a, b) };
    i64::from(
        text(&a.inner, &b.inner)
            && ints(&a.i64_inner, &b.i64_inner)
            && aggregates(&a.struct_inner, &b.struct_inner),
    )
}

fn aggregate_union(a: &AggregateTable, b: &AggregateTable) -> AggregateTable {
    let mut out = a.clone();
    for (key, slots) in b {
        out.entry(key.clone()).or_insert_with(|| slots.clone());
    }
    out
}

fn aggregate_intersection(a: &AggregateTable, b: &AggregateTable) -> AggregateTable {
    a.iter()
        .filter(|(key, _)| b.contains_key(key.as_ref()))
        .map(|(key, slots)| (key.clone(), slots.clone()))
        .collect()
}

fn aggregate_difference(a: &AggregateTable, b: &AggregateTable) -> AggregateTable {
    a.iter()
        .filter(|(key, _)| !b.contains_key(key.as_ref()))
        .map(|(key, slots)| (key.clone(), slots.clone()))
        .collect()
}

/// Inserts a struct or tuple by value. `desc` is the same slot descriptor as
/// the aggregate-keyed HashMap ABI, so equal values at different addresses
/// remain equal in the native runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_insert_skey(
    s: *mut GosSet,
    key: *const u8,
    desc: *const c_char,
) -> i64 {
    ffi_entry!(-1, {
        let Some(canonical) = (unsafe { crate::c_abi::map::build_skey_for_set(key, desc) }) else {
            return 0;
        };
        if s.is_null() {
            return 0;
        }
        let width = unsafe { crate::c_abi::gos_str_arg_len(desc) } * 8;
        let slots = unsafe { std::slice::from_raw_parts(key, width) }
            .to_vec()
            .into_boxed_slice();
        let s = unsafe { &mut *s };
        i64::from(
            s.struct_inner
                .insert(canonical.into_boxed_slice(), slots)
                .is_none(),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_contains_skey(
    s: *const GosSet,
    key: *const u8,
    desc: *const c_char,
) -> i64 {
    ffi_entry!(-1, {
        let Some(canonical) = (unsafe { crate::c_abi::map::build_skey_for_set(key, desc) }) else {
            return 0;
        };
        if s.is_null() {
            return 0;
        }
        let s = unsafe { &*s };
        i64::from(s.struct_inner.contains_key(canonical.as_slice()))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_remove_skey(
    s: *mut GosSet,
    key: *const u8,
    desc: *const c_char,
) -> i64 {
    ffi_entry!(-1, {
        let Some(canonical) = (unsafe { crate::c_abi::map::build_skey_for_set(key, desc) }) else {
            return 0;
        };
        if s.is_null() {
            return 0;
        }
        let s = unsafe { &mut *s };
        i64::from(s.struct_inner.shift_remove(canonical.as_slice()).is_some())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_to_vec_skey(
    s: *const GosSet,
    desc: *const c_char,
) -> *mut crate::c_abi::vec::GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        if desc.is_null() {
            return std::ptr::null_mut();
        }
        let width = unsafe { crate::c_abi::gos_str_arg_len(desc) } * 8;
        let out = unsafe {
            crate::c_abi::vec::gos_rt_vec_new_typed(
                width as u32,
                crate::c_abi::vec::vec_elem_kind::PRIMITIVE,
            )
        };
        if s.is_null() {
            return out;
        }
        let s = unsafe { &*s };
        let mut entries: Vec<_> = s.struct_inner.iter().collect();
        if s.ordered {
            entries.sort_unstable_by_key(|(key, _)| *key);
        }
        for (_, slots) in entries {
            unsafe { crate::c_abi::vec::gos_rt_vec_push(out, slots.as_ptr()) };
        }
        out
    })
}

/// Aggregate-key counterpart of [`gos_rt_set_intersection_to_vec`]. It reads in
/// the receiver's own order while avoiding the cloned keys and slots of an
/// intermediate intersection set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_intersection_to_vec_skey(
    a: *const GosSet,
    b: *const GosSet,
    desc: *const c_char,
) -> *mut crate::c_abi::vec::GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        if desc.is_null() {
            return std::ptr::null_mut();
        }
        let width = unsafe { crate::c_abi::gos_str_arg_len(desc) } * 8;
        let out = unsafe {
            crate::c_abi::vec::gos_rt_vec_new_typed(
                width as u32,
                crate::c_abi::vec::vec_elem_kind::PRIMITIVE,
            )
        };
        if a.is_null() || b.is_null() {
            return out;
        }
        let (a, b) = unsafe { (&*a, &*b) };
        let mut entries: Vec<_> = a
            .struct_inner
            .iter()
            .filter(|(key, _)| b.struct_inner.contains_key(key.as_ref()))
            .collect();
        if a.ordered {
            entries.sort_unstable_by_key(|(key, _)| *key);
        }
        for (_, slots) in entries {
            unsafe { crate::c_abi::vec::gos_rt_vec_push(out, slots.as_ptr()) };
        }
        out
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_union(a: *const GosSet, b: *const GosSet) -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        unsafe {
            set_combine(
                a,
                b,
                |x, y| x.union(y).cloned().collect(),
                |x, y| x.union(y).copied().collect(),
                aggregate_union,
            )
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_intersection(
    a: *const GosSet,
    b: *const GosSet,
) -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        unsafe {
            set_combine(
                a,
                b,
                |x, y| x.intersection(y).cloned().collect(),
                |x, y| x.intersection(y).copied().collect(),
                aggregate_intersection,
            )
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_intersection_skey(
    a: *const GosSet,
    b: *const GosSet,
) -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        unsafe {
            set_combine(
                a,
                b,
                |x, y| x.intersection(y).cloned().collect(),
                |x, y| x.intersection(y).copied().collect(),
                aggregate_intersection,
            )
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_difference(a: *const GosSet, b: *const GosSet) -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        unsafe {
            set_combine(
                a,
                b,
                |x, y| x.difference(y).cloned().collect(),
                |x, y| x.difference(y).copied().collect(),
                aggregate_difference,
            )
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_symmetric_difference(
    a: *const GosSet,
    b: *const GosSet,
) -> *mut GosSet {
    ffi_entry!(std::ptr::null_mut(), {
        unsafe {
            set_combine(
                a,
                b,
                |x, y| x.symmetric_difference(y).cloned().collect(),
                |x, y| x.symmetric_difference(y).copied().collect(),
                |x, y| {
                    let mut out = aggregate_difference(x, y);
                    out.extend(aggregate_difference(y, x));
                    out
                },
            )
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_is_subset(a: *const GosSet, b: *const GosSet) -> i64 {
    ffi_entry!(-1, {
        unsafe {
            set_relation(a, b, SetTable::is_subset, SetTable::is_subset, |x, y| {
                x.keys().all(|key| y.contains_key(key.as_ref()))
            })
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_is_superset(a: *const GosSet, b: *const GosSet) -> i64 {
    ffi_entry!(-1, {
        unsafe {
            set_relation(
                a,
                b,
                SetTable::is_superset,
                SetTable::is_superset,
                |x, y| y.keys().all(|key| x.contains_key(key.as_ref())),
            )
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_is_disjoint(a: *const GosSet, b: *const GosSet) -> i64 {
    ffi_entry!(-1, {
        unsafe {
            set_relation(
                a,
                b,
                SetTable::is_disjoint,
                SetTable::is_disjoint,
                |x, y| !x.keys().any(|key| y.contains_key(key.as_ref())),
            )
        }
    })
}
