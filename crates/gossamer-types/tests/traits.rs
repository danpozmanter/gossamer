//! End-to-end tests for the trait resolver.

use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{ImplIndex, IntTy, TraitError, TyCtxt, TyKind};

struct Built {
    index: ImplIndex,
    diagnostics: Vec<gossamer_types::TraitDiagnostic>,
    tcx: TyCtxt,
}

fn build(source: &str) -> Built {
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse errors: {parse_diags:?}");
    let (resolutions, _resolve_diags) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (index, diagnostics) = ImplIndex::build(&sf, &resolutions, &mut tcx);
    Built {
        index,
        diagnostics,
        tcx,
    }
}

#[test]
fn inherent_method_is_findable() {
    let source = r"
struct Counter { hits: i32 }

impl Counter {
    fn bump(&self) -> i32 { 0i32 }
    fn total(&self) -> i32 { 0i32 }
}
";
    let mut built = build(source);
    assert!(built.diagnostics.is_empty(), "{:?}", built.diagnostics);
    let counter_ty = built.tcx.intern(TyKind::Adt {
        def: gossamer_resolve::DefId::local(0),
        substs: gossamer_types::Substs::new(),
    });
    let _ = counter_ty;
    let entry = built.index.entries().next().expect("impl present").1;
    assert_eq!(entry.methods.len(), 2);
    let bump = built
        .index
        .resolve_inherent_method(entry.self_ty, "bump")
        .expect("bump resolves");
    assert_eq!(bump.method_slot, 0);
    let total = built
        .index
        .resolve_inherent_method(entry.self_ty, "total")
        .expect("total resolves");
    assert_eq!(total.method_slot, 1);
    assert!(
        built
            .index
            .resolve_inherent_method(entry.self_ty, "missing")
            .is_none()
    );
}

#[test]
fn trait_method_resolves_via_trait_impl() {
    let source = r"
trait Greet {
    fn hello(&self) -> i32
}

struct Foo

impl Greet for Foo {
    fn hello(&self) -> i32 { 1i32 }
}
";
    let built = build(source);
    assert!(built.diagnostics.is_empty(), "{:?}", built.diagnostics);
    let impl_entry = built
        .index
        .entries()
        .map(|(_, entry)| entry)
        .find(|entry| entry.trait_ref.is_some())
        .expect("trait impl present");
    let resolution = built
        .index
        .resolve_method(impl_entry.self_ty, "hello")
        .expect("hello resolved");
    assert_eq!(resolution.method_slot, 0);
}

#[test]
fn overlapping_trait_impls_are_flagged() {
    let source = r"
trait Tag {
    fn tag(&self) -> i32
}

struct Foo

impl Tag for Foo {
    fn tag(&self) -> i32 { 0i32 }
}

impl Tag for Foo {
    fn tag(&self) -> i32 { 1i32 }
}
";
    let built = build(source);
    assert!(
        built
            .diagnostics
            .iter()
            .any(|d| matches!(d.error, TraitError::OverlappingImpls { .. })),
        "expected overlapping-impls diagnostic: {:?}",
        built.diagnostics
    );
}

#[test]
fn vtable_is_built_in_trait_declaration_order() {
    let source = r"
trait Api {
    fn alpha(&self) -> i32
    fn beta(&self) -> i32
}

struct Target

impl Api for Target {
    fn beta(&self) -> i32 { 2i32 }
    fn alpha(&self) -> i32 { 1i32 }
}
";
    let built = build(source);
    assert!(built.diagnostics.is_empty(), "{:?}", built.diagnostics);
    let (impl_id, entry) = built
        .index
        .entries()
        .find(|(_, entry)| entry.trait_ref.is_some())
        .expect("trait impl present");
    let vtable = built.index.vtable(impl_id).expect("vtable built");
    assert_eq!(vtable.len(), 2);
    let alpha_id = entry.methods.iter().find(|m| m.name == "alpha").unwrap().id;
    let beta_id = entry.methods.iter().find(|m| m.name == "beta").unwrap().id;
    assert_eq!(vtable[0], alpha_id);
    assert_eq!(vtable[1], beta_id);
}

#[test]
fn trait_entry_tracks_method_names() {
    let source = r"
trait Serde {
    fn encode(&self) -> i32
    fn decode(&self) -> i32
}
";
    let built = build(source);
    let entry = built.index.traits().next().expect("trait registered");
    assert_eq!(entry.name, "Serde");
    assert_eq!(
        entry.methods,
        vec!["encode".to_string(), "decode".to_string()]
    );
}

#[test]
fn method_signature_lowers_primitive_types() {
    let source = r"
struct Target

impl Target {
    fn width(&self) -> i32 { 0i32 }
}
";
    let mut built = build(source);
    let entry = built.index.entries().next().expect("impl present").1;
    let method = &entry.methods[0];
    let i32_ty = built.tcx.int_ty(IntTy::I32);
    assert_eq!(method.sig.output, i32_ty);
    assert!(method.has_self);
}
