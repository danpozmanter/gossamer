//! Catches the C7 finding (VM inline-cache slots never invalidate
//! when globals are reassigned) from
//! `~/dev/contexts/lang/adversarial_analysis.md`.

use gossamer_interp::Vm;

#[test]
fn fresh_vm_starts_with_generation_one() {
    let vm = Vm::new();
    assert_eq!(
        vm.globals_generation(),
        1,
        "Vm::new must seed generation = 1 so the empty-slot sentinel \
         (`generation == 0`) never matches a freshly populated cache"
    );
}

#[test]
fn bump_generation_increases_monotonically() {
    let vm = Vm::new();
    let g0 = vm.globals_generation();
    let g1 = vm.bump_globals_generation();
    let g2 = vm.bump_globals_generation();
    assert_eq!(g1, g0.wrapping_add(1));
    assert_eq!(g2, g1.wrapping_add(1));
    assert_ne!(g0, g1);
    assert_ne!(g1, g2);
    assert_eq!(vm.globals_generation(), g2);
}

#[test]
fn bump_skips_zero_on_wrap_so_empty_slot_sentinel_stays_distinct() {
    let vm = Vm::new();
    vm.set_globals_generation_for_test(u32::MAX);
    let next = vm.bump_globals_generation();
    assert_ne!(
        next, 0,
        "wrap from u32::MAX must skip the empty-slot 0 value"
    );
    assert_eq!(next, 1);
}
