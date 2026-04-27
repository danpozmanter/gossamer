//! Runtime support library linked into every Gossamer program.
//! Commits the compiler and runtime to a single value layout.
//! will add the tracing GC on top of the allocator implied
//! here; the scheduler. For now this crate exposes the
//! layout descriptors in [`layout`] so the rest of the toolchain can
//! assume a stable representation.

// `c_abi` requires unsafe for `#[no_mangle] extern "C"` symbols and
// raw-pointer dispatch. The rest of the crate stays safe by
// scoping unsafe blocks inside that module.

pub mod builtins;
pub mod c_abi;
pub mod layout;
pub mod safe_env;
pub mod value;

pub use layout::{HEAP_ALIGN, ObjHeader, Ptr, TypeInfo, WORD_BYTES, header_align, header_size};
pub use value::{
    GossamerValue, SINGLETON_FALSE, SINGLETON_TRUE, SINGLETON_UNIT, TAG_FLOAT, TAG_HEAP,
    TAG_IMMEDIATE, TAG_MASK, TAG_SINGLETON, fits_i56, from_f64, from_heap_handle, from_i64,
    from_singleton, tag_of, to_f64, to_heap_handle, to_i64, to_singleton,
};
