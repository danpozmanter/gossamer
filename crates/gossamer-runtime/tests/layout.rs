//! Layout snapshot tests — any change to sizes/alignments here is a
//! coordinated ABI-level decision that must match the Cranelift
//! backend's assumptions.

use std::mem::{align_of, size_of};

use gossamer_runtime::layout::{
    HEAP_ALIGN, ObjHeader, Ptr, WORD_BYTES, closure, dyn_ref, hashmap, header_align, header_size,
    string, vec,
};

#[test]
fn object_header_is_two_words() {
    assert_eq!(size_of::<ObjHeader>(), 2 * WORD_BYTES);
    assert_eq!(header_size(), 16);
    assert_eq!(header_align(), WORD_BYTES);
}

#[test]
fn pointer_is_word_sized() {
    assert_eq!(size_of::<Ptr>(), WORD_BYTES);
    assert_eq!(align_of::<Ptr>(), WORD_BYTES);
}

#[test]
fn string_and_vec_reprs_match_three_words() {
    assert_eq!(size_of::<string::Repr>(), 3 * WORD_BYTES);
    assert_eq!(size_of::<vec::Repr>(), 3 * WORD_BYTES);
}

#[test]
fn hashmap_repr_holds_four_words() {
    assert_eq!(size_of::<hashmap::Repr>(), 4 * WORD_BYTES);
}

#[test]
fn fat_pointer_representations_are_two_words() {
    assert_eq!(size_of::<dyn_ref::Repr>(), 2 * WORD_BYTES);
    assert_eq!(size_of::<closure::Repr>(), 2 * WORD_BYTES);
}

#[test]
fn heap_alignment_matches_word_size() {
    assert_eq!(HEAP_ALIGN, WORD_BYTES);
}
