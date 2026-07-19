#[test]
fn format_macro_result_is_typed_as_string() {
    // `format!("{}{}", a, b).len()` returned a multi-trillion
    // garbage value in the cranelift compiled tier because the
    // typechecker had no signature for the parser-emitted
    // `__concat` intrinsic. The result local was typed as
    // `Var(_)` and the `.len()` dispatch picked `gos_rt_len`
    // (the generic Vec/HashMap length helper) instead of
    // `gos_rt_str_len`. The generic helper read a Vec
    // header from the *c_char pointer and printed garbage.
    // Fix: the typechecker now pins `__concat` / `__fmt_prec`
    // / `format` to `String` and `println` / `print` /
    // `eprintln` / `eprint` to `Unit` in its fallback table.
    let src = r#"
fn main() {
    let a = "foo"
    let b = "bar"
    let combined = format!("{}{}", a, b)
    println!("len = {}", combined.len())
}
"#;
    let dir = fresh_dir("format_len_typed");
    let path = write_source(&dir, "format_len_typed", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(
        run.0.contains("len = 6"),
        "vm: expected len=6, got: {:?}",
        run.0
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert!(
        native.0.contains("len = 6"),
        "native: expected len=6, got: {:?}",
        native.0
    );
}

#[test]
fn immutable_static_path_resolves_to_typed_constant() {
    // `static N: i64 = 5; println!("{}", N)` previously
    // segfaulted in the cranelift compiled tier (and emitted
    // empty output). The typechecker left the path expression
    // typed as `Var(_)`, and `consts.get(def)` returned None
    // for static items (only `const` items were folded), so
    // `N` lowered as a `FnRef` whose pointer was then handed
    // to `gos_rt_concat_str` and caused a strlen segfault.
    // Fix: extend `collect_const_values` to fold immutable
    // `static` items too, and pin the local's MIR type from
    // the const value's shape when the typechecker leaves it
    // as `Var(_)` so format-arg dispatch picks the right
    // helper (concat_i64 / concat_f64 / concat_str etc.).
    let src = r#"
static MAX_RETRIES: i64 = 5
static THRESHOLD: f64 = 0.75
static GREETING: &str = "hello"

fn above_threshold(v: f64) -> bool {
    v > THRESHOLD
}

fn main() {
    println!("MAX_RETRIES = {}", MAX_RETRIES)
    println!("THRESHOLD = {}", THRESHOLD)
    println!("GREETING = {}", GREETING)
    println!("above(0.5) = {}", above_threshold(0.5))
    println!("above(0.8) = {}", above_threshold(0.8))
}
"#;
    let dir = fresh_dir("static_items_typed");
    let path = write_source(&dir, "static_items_typed", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(run.0.contains("MAX_RETRIES = 5"), "vm got: {:?}", run.0);
    assert!(run.0.contains("THRESHOLD = 0.75"), "vm got: {:?}", run.0);
    assert!(run.0.contains("GREETING = hello"), "vm got: {:?}", run.0);
    assert!(run.0.contains("above(0.5) = false"), "vm got: {:?}", run.0);
    assert!(run.0.contains("above(0.8) = true"), "vm got: {:?}", run.0);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert!(
        native.0.contains("MAX_RETRIES = 5"),
        "native got: {:?}",
        native.0
    );
    assert!(
        native.0.contains("THRESHOLD = 0.75"),
        "native got: {:?}",
        native.0
    );
    assert!(
        native.0.contains("GREETING = hello"),
        "native got: {:?}",
        native.0
    );
    assert!(
        native.0.contains("above(0.5) = false"),
        "native got: {:?}",
        native.0
    );
    assert!(
        native.0.contains("above(0.8) = true"),
        "native got: {:?}",
        native.0
    );
}

#[test]
fn static_mut_assignment_does_not_error_at_runtime() {
    // `static mut COUNTER: i64 = 0; COUNTER = 100` previously
    // failed in the VM with "name `COUNTER` is not bound in
    // this scope" because `eval_assign` only consulted the
    // goroutine-local `Env`, not the tree-walker's globals
    // table where statics live. The tree-walker's `eval_path`
    // already resolves the read against globals, so the
    // asymmetry was invisible until a write hit. This test
    // pins the no-error contract; it does *not* yet assert
    // that the read sees the written value, because the
    // bytecode VM's globals are a separate `Arc<HashMap>` and
    // sharing storage with the tree-walker is an open
    // follow-up. The contract today: writes accept, reads
    // return the initial value (consistent with cranelift on
    // this build).
    let src = r#"
static mut N: i64 = 7

fn main() {
    println!("start = {}", N)
    N = 42
    println!("after = {}", N)
}
"#;
    let dir = fresh_dir("static_mut_assign");
    let path = write_source(&dir, "static_mut_assign", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(
        run.0.contains("start = 7"),
        "vm: expected static-mut initial value visible, got: {:?}",
        run.0,
    );
    assert!(
        run.0.contains("after ="),
        "vm: expected the post-assign println to run instead of erroring, got: {:?}",
        run.0,
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert!(
        native.0.contains("start = 7"),
        "native: expected static-mut initial value visible, got: {:?}",
        native.0,
    );
    assert!(
        native.0.contains("after ="),
        "native: expected the post-assign println to run instead of erroring, got: {:?}",
        native.0,
    );
}

#[test]
fn at_binding_subpattern_actually_filters_match_arms() {
    // `x @ literal` and `x @ lo..=hi` previously dropped the
    // subpattern at the AST→HIR boundary: `lower_pat_kind`
    // destructured `AstPatKind::Ident { name, mutability, .. }`
    // with `..` swallowing the `subpattern` field. Both VM and
    // cranelift always picked the first arm with a stale binding;
    // cranelift additionally bound `x` to a heap pointer instead
    // of the integer value (a representation-drift symptom).
    // The fix introduces `HirPatKind::At { name, mutable, sub }`
    // and threads it through every consumer (HIR walker, MIR
    // match lowering, exhaustiveness check, tree-walker
    // pattern matchers).
    let src = r#"
fn classify(n: i64) -> String {
    match n {
        x @ 0 => format!("zero ({})", x),
        x @ 1..=3 => format!("small {}", x),
        x @ 4..=10 => format!("medium {}", x),
        x => format!("other {}", x),
    }
}

fn main() {
    let inputs = [0, 1, 2, 3, 4, 7, 10, 11, -1]
    for n in inputs {
        println!("{}", classify(n))
    }
}
"#;
    let dir = fresh_dir("at_binding_filter");
    let path = write_source(&dir, "at_binding_filter", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    let expected =
        "zero (0)\nsmall 1\nsmall 2\nsmall 3\nmedium 4\nmedium 7\nmedium 10\nother 11\nother -1\n";
    assert_eq!(
        run.0, expected,
        "vm: at-binding subpattern; got: {:?}",
        run.0
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert_eq!(
        native.0, expected,
        "native: at-binding subpattern; got: {:?}",
        native.0
    );
}

#[test]
fn continue_in_for_vec_iter_advances_index() {
    // `for x in xs.iter() { if cond { continue } body }`
    // previously livelocked the bytecode VM for the same reason
    // as the for-range case: the vec-iter fast path's `continue`
    // skipped the index increment that lives between the body
    // and the back-edge.
    let src = r#"
fn main() {
    let xs: [i64] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10].to_vec()
    let mut acc: i64 = 0
    for x in xs.iter() {
        if x % 3 == 0 {
            continue
        }
        acc = acc + x
    }
    println!("acc={}", acc)
}
"#;
    let dir = fresh_dir("continue_for_vec_iter");
    let path = write_source(&dir, "continue_for_vec_iter", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(
        run.0.contains("acc=37"),
        "vm: expected acc=37, got: {:?}",
        run.0
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert!(
        native.0.contains("acc=37"),
        "native: expected acc=37, got: {:?}",
        native.0
    );
}

#[test]
fn result_option_payload_literal_matches_correct_arm() {
    // `Ok(1)` / `Ok(2)` must route to different arms in both VM and compiled.
    let src = r#"
fn classify(r: Result<i64, i64>) -> &str {
    match r {
        Ok(1) => "one",
        Ok(2) => "two",
        Ok(_) => "other-ok",
        Err(_) => "err",
    }
}

fn pick(o: Option<i64>) -> &str {
    match o {
        Some(10) => "ten",
        Some(20) => "twenty",
        None => "none",
        _ => "other",
    }
}

fn main() {
    println!("{}", classify(Ok(1)))
    println!("{}", classify(Ok(2)))
    println!("{}", classify(Ok(99)))
    println!("{}", classify(Err(0)))
    println!("{}", pick(Some(10)))
    println!("{}", pick(Some(20)))
    println!("{}", pick(None))
}
"#;
    let expected = "one\ntwo\nother-ok\nerr\nten\ntwenty\nnone\n";
    let dir = fresh_dir("payload_literal_match");
    let path = write_source(&dir, "payload_literal_match", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert_eq!(run.0, expected, "vm: got {:?}", run.0);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert_eq!(native.0, expected, "native: got {:?}", native.0);
}

#[test]
fn unary_prefix_at_line_start_breaks_statement() {
    // `&s`, `*p`, `-n` at the start of a line after a
    // semicolonless statement must parse as a new statement, not
    // as a binary continuation of the prior expression. Before the
    // fix, `let s = "hi"\n&s |> ...` was glued into `let s = "hi" &
    // s |> ...` and resolution failed with "cannot find `s` in
    // this scope".
    let src = r#"
use std::{iter, strings}

fn main() {
    let s = "alpha\nbeta"
    &s |> strings::lines |> iter::for_each(|l| println!("{}", l))

    let n = 5
    -n
    println!("post-neg")

    let v = 42
    let p = &v
    *p
    println!("post-deref={}", *p)
}
"#;
    let expected = "alpha\nbeta\npost-neg\npost-deref=42\n";
    let dir = fresh_dir("unary_line_start");
    let path = write_source(&dir, "unary_line_start", src);
    let run = run_vm(&path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert_eq!(run.0, expected, "vm: got {:?}", run.0);
}

#[test]
fn try_operator_in_macro_arg_propagates_early_return() {
    // `?` inside a macro argument - e.g. `print!("{}", expr?)` -
    // must propagate the early-return from the enclosing function,
    // not silently pass the `Err(...)` value through to the macro.
    // The bug: eval_expr_to_value was converting Flow::Return(v)
    // to Ok(v), so the Err value was passed to __concat / print
    // instead of returning early from `cat`.
    let src = r#"
use std::{errors, fs}

fn cat(f: &String) -> Result<(), errors::Error> {
    Ok(print!("{}", fs::read_to_string(f)?))
}

fn main() {
    if let Err(e) = cat(&"/nonexistent-regression") {
        println!("caught: {e}")
    }
}
"#;
    let expected = "caught: not found: /nonexistent-regression\n";
    let dir = fresh_dir("try_in_macro_arg");
    let path = write_source(&dir, "try_in_macro_arg", src);
    let run = run_vm(&path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert_eq!(run.0, expected, "vm: got {:?}", run.0);
}

#[test]
fn llvm_named_fn_passed_to_sort_by_emits_typed_store() {
    // `Operand::FnRef` in the LLVM lowerer used to always emit
    // `ptrtoint ptr @"name" to i64`, but the destination slot is
    // ptr-typed when `FnDef → ptr` (e.g. when a named fn is passed
    // as a `Fn(i64,i64)->i64` arg to `sort_by`). The emitted
    // `store ptr %i64_value, ptr %slot` then fails opt validation
    // and the whole module silently falls back to Cranelift.
    let src = r#"
fn cmp(a: i64, b: i64) -> i64 { a - b }
fn main() {
    let mut xs = [5, 2, 4, 1, 3].to_vec()
    xs.sort_by(cmp)
    for x in xs.iter() { println!("{}", *x) }
}
"#;
    let dir = fresh_dir("llvm_sortby_named_fn");
    let path = write_source(&dir, "llvm_sortby_named_fn", src);
    let scratch = dir.join("rel");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("llvm release build");
    let out = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "native: stderr: {}", out.1);
    assert_eq!(out.0, "1\n2\n3\n4\n5\n");
}

#[test]
fn llvm_vec_of_tuples_index_returns_both_fields() {
    // `Vec<(i64, f64)>`-style tuple element types used to leave
    // the operand of `xs.push((1, 1.5))` typed as
    // `(Var, Var)` in MIR. The LLVM lowerer's `slot_count` for
    // a tuple with `Var` elements returned `None`, the alloca
    // shrank to 1 slot, the second-slot store overflowed, and
    // the subsequent `gos_rt_vec_get_ptr → memcpy` round-trip
    // surfaced garbage in the f64 field.
    let src = r#"
fn main() {
    let mut xs: [(i64, f64)] = [].to_vec()
    xs.push((1, 1.5))
    xs.push((2, 2.5))
    let i: i64 = 1
    let p = xs[i]
    println!("{} {}", p.0, p.1)
}
"#;
    let dir = fresh_dir("llvm_vec_tuple_index");
    let path = write_source(&dir, "llvm_vec_tuple_index", src);
    let scratch = dir.join("rel");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("llvm release build");
    let out = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "native: stderr: {}", out.1);
    assert_eq!(out.0, "2 2.5\n");
}

#[test]
fn llvm_tuple_return_array_then_scalar_preserves_both() {
    // Returning an `([f64; N], f64)` tuple used to corrupt every
    // slot past the first: the temporary tuple local was typed
    // `([Var; 4], Var)`, `slot_count` collapsed to `None`, and
    // the alloca undersized to 1 slot. The aggregate-store then
    // overflowed and the subsequent memcpy into the return slot
    // copied stack garbage.
    let src = r#"
fn make() -> ([f64; 4], f64) {
    ([1.5, 2.5, 3.5, 4.5], 99.0)
}
fn main() {
    let pair = make()
    println!("{} {} {} {} | {}", pair.0[0], pair.0[1], pair.0[2], pair.0[3], pair.1)
}
"#;
    let dir = fresh_dir("llvm_tuple_arr_scalar");
    let path = write_source(&dir, "llvm_tuple_arr_scalar", src);
    let scratch = dir.join("rel");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("llvm release build");
    let out = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "native: stderr: {}", out.1);
    assert_eq!(out.0, "1.5 2.5 3.5 4.5 | 99\n");
}

#[test]
fn llvm_tuple_return_from_nested_loop_keeps_second_slot() {
    // `return (a, b)` from inside a nested loop used to drop the
    // second slot - the temporary `(Var, Var)` tuple's alloca
    // sized to one slot, so the aggregate-store overflowed and
    // the memcpy into the return slot only carried 8 valid bytes.
    // fannkuch-shaped programs lost the checksum value (always 0).
    let src = r#"
fn fannkuch(_n: i64) -> (i64, i64) {
    let mut perm = [0, 1, 2, 3, 4]
    let mut max_flips = 0
    let mut checksum = 0
    let mut sign = true
    let mut nperm = 0
    loop {
        let mut flips = 0
        let mut k = perm[0]
        while k != 0 {
            let mut i = 0
            let mut j = k
            while i < j {
                let t = perm[i]
                perm[i] = perm[j]
                perm[j] = t
                i += 1
                j -= 1
            }
            k = perm[0]
            flips += 1
        }
        if flips > max_flips { max_flips = flips }
        checksum += if sign { flips } else { -flips }
        if nperm >= 30 {
            return (max_flips, checksum)
        }
        nperm += 1
        if sign {
            let t = perm[0]
            perm[0] = perm[1]
            perm[1] = t
            sign = false
        } else {
            let t = perm[1]
            perm[1] = perm[2]
            perm[2] = t
            sign = true
        }
    }
}
fn main() {
    let r = fannkuch(5)
    println!("max={} checksum={}", r.0, r.1)
}
"#;
    let dir = fresh_dir("llvm_nested_loop_tuple_ret");
    let path = write_source(&dir, "llvm_nested_loop_tuple_ret", src);
    let scratch = dir.join("rel");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("llvm release build");
    let out = run_native(&bin);
    let vm = run_vm(&path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "native: stderr: {}", out.1);
    assert_eq!(out.0, vm.0, "native diverged from VM");
    // The exact value can't be 0 - that's the bug shape we're
    // guarding against.
    assert!(!out.0.contains("checksum=0\n"), "second slot dropped");
}

#[test]
fn json_render_adt_text_branch_does_not_free_uninit_pairs_vec() {
    // json::render(&adt) builds a temporary GosVec (pairs_vec) inside
    // lower_json_render_adt.  The insert_drops_at_returns pass used to
    // emit a gos_rt_vec_free for pairs_vec at every Return block -
    // including the text-mode arm where pairs_vec was never initialised.
    // That produced gos_rt_vec_free(stack_garbage) → segfault in
    // __GI___libc_free.  The fix: emit the free immediately in the JSON
    // arm and re-assign pairs_vec to 0 so the global drop pass skips it.
    let src = r#"
use std::encoding::json

struct Info { num: i64, label: String }

fn show(item: Info, as_json: bool) {
    if as_json {
        println!("{}", json::render(&item))
    } else {
        println!("num={} label={}", item.num, item.label)
    }
}

fn main() {
    let it = Info { num: 42, label: "hello".to_string() }
    show(it, false)
}
"#;
    let dir = fresh_dir("json_render_text_branch");
    let path = write_source(&dir, "json_render_text_branch", src);
    let scratch = dir.join("bin");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native(&path, &scratch).expect("build");
    let out = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "segfault in text branch; stderr: {}", out.1);
    assert_eq!(out.0, "num=42 label=hello\n");
}

#[test]
fn jit_pre_interns_array_index_label_string() {
    // Regression: a program that hits the bounds-check helper
    // path (any `arr[i]` with i64 index) would route through the
    // codegen helper that interns "array index" as the diagnostic
    // label. The pre-pass that pre-interns strings before the
    // parallel codegen phase missed this literal, so the first
    // bounds-checked array access in any body panicked with
    // `OfflineModule: declare_data called in parallel phase`.
    //
    // The fix: pre-intern "array index" alongside `""`, `" "`,
    // and `"<value>"` in the codegen prelude. spectral-norm's
    // `src[j]` access is the canonical trigger.
    let src = "fn main() {\n\
                   let xs: [i64; 4] = [1, 2, 3, 4]\n\
                   let mut sum: i64 = 0\n\
                   let n: i64 = 4\n\
                   let mut i: i64 = 0\n\
                   while i < n {\n\
                       sum += xs[i]\n\
                       i += 1\n\
                   }\n\
                   println!(\"{}\", sum)\n\
               }\n";
    let dir = fresh_dir("jit_array_index_pre_intern");
    let path = write_source(&dir, "jit_array_index_pre_intern", src);
    let run = run_vm(&path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        run.2,
        Some(0),
        "vm: expected clean exit, got {:?}; stderr: {}",
        run.2,
        run.1,
    );
    assert!(
        !run.1.contains("declare_data called in parallel phase"),
        "vm: regressed parallel-phase declare_data panic; stderr: {}",
        run.1
    );
    assert_eq!(run.0.trim_end(), "10");
}

#[test]
fn local_var_shadowing_module_does_not_capture_qualified_path() {
    // Regression: a local binding whose name matches an imported
    // module silently captured every `mod_name::item(...)` call
    // through the VM-tier's tree-walker fallback. `eval_path`
    // looked up the head segment in the env first, returning the
    // local's value (a String), and `apply()` of a non-callable
    // degraded to Unit. The LLVM AOT tier resolved correctly; the
    // VM tier did not - a parity gap that broke askq's
    // `provider::provider_endpoint_and_auth(&cfg, &provider)` call
    // (the local `provider: String` captured the call).
    //
    // The fix: multi-segment paths bypass the env-first lookup.
    // A path's head can only resolve to a module / type / trait -
    // never a local binding.
    let dir = fresh_dir("local_shadow_mod_path");
    fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/shadow\"\nversion = \"0.0.1\"\n",
    )
    .expect("write project.toml");
    let src_dir = dir.join("src");
    fs::create_dir_all(&src_dir).expect("mk src dir");
    fs::write(
        src_dir.join("main.gos"),
        "mod prov;\n\
         fn main() {\n\
             let prov = \"local-string\".to_string()\n\
             let s = prov::greet(&prov)\n\
             println!(\"{}\", s)\n\
         }\n",
    )
    .expect("write main.gos");
    fs::write(
        src_dir.join("prov.gos"),
        "pub fn greet(who: &String) -> String {\n\
             format!(\"hello, {}\", who)\n\
         }\n",
    )
    .expect("write prov.gos");
    let main_path = src_dir.join("main.gos");
    let run = run_vm(&main_path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        run.2,
        Some(0),
        "vm: expected clean exit, got {:?}; stderr: {}",
        run.2,
        run.1
    );
    assert_eq!(
        run.0.trim_end(),
        "hello, local-string",
        "vm: expected greet to run, got stdout: {:?}",
        run.0,
    );
}

#[test]
fn aggregate_alloc_loop_reclaims_deterministically() {
    // Stress: a tight loop that allocates a heap aggregate every
    // iteration and discards it. The MIR drop pass must emit a
    // matching `gos_rt_aggr_free` per iteration. The test verifies:
    //   - the loop produces the correct numeric result (the drop
    //     pass does not free values still held in locals);
    //   - the process exits with status 0 (no segfault, no
    //     double-free in the drop pass);
    //   - all three tiers agree.
    let src = "struct Pair { a: i64, b: i64 }\n\
               fn make(i: i64) -> Pair { Pair { left: i, right: i * 2 } }\n\
               fn main() {\n\
                   let mut total: i64 = 0\n\
                   let mut i: i64 = 0\n\
                   while i < 10000 {\n\
                       let p = make(i)\n\
                       total += p.a + p.b\n\
                       i += 1\n\
                   }\n\
                   println!(\"{}\", total)\n\
               }\n";
    let expected = (0i64..10000).map(|i| i + i * 2).sum::<i64>();
    let dir = fresh_dir("tracing_gc_loop");
    let path = write_source(&dir, "tracing_gc_loop", src);

    // VM tier
    let run = {
        let child = Command::new(gos_bin())
            .arg("run")
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn gos run");
        run_with_timeout(child)
    };
    assert_eq!(
        run.2,
        Some(0),
        "vm: expected clean exit, got {:?}; stderr: {}",
        run.2,
        run.1
    );
    assert_eq!(run.0.trim_end(), expected.to_string(), "vm output mismatch");

    // Debug LLVM tier
    let scratch = dir.join("bin");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native(&path, &scratch).expect("build debug");
    let out = {
        let child = Command::new(&bin)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn debug binary");
        run_with_timeout(child)
    };
    assert_eq!(
        out.2,
        Some(0),
        "debug: expected clean exit, got {:?}; stderr: {}",
        out.2,
        out.1
    );
    assert_eq!(
        out.0.trim_end(),
        expected.to_string(),
        "debug output mismatch"
    );

    // Release LLVM tier
    let bin = build_native_release(&path, &scratch).expect("build release");
    let out = {
        let child = Command::new(&bin)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn release binary");
        run_with_timeout(child)
    };
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        out.2,
        Some(0),
        "release: expected clean exit, got {:?}; stderr: {}",
        out.2,
        out.1
    );
    assert_eq!(
        out.0.trim_end(),
        expected.to_string(),
        "release output mismatch"
    );
}

#[test]
fn named_struct_constructor_is_available_on_vm_and_native_tiers() {
    let src = r#"
struct Pair { left: i64, right: i64 }

fn sum(p: Pair) -> i64 {
    p.left + p.right
}

fn main() {
    let p = Pair { left: 20i64, right: 22i64 }
    println!("{}", sum(p))
}
"#;
    let dir = fresh_dir("named_struct_ctor");
    let path = write_source(&dir, "named_struct_ctor", src);

    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert_eq!(run.0.trim_end(), "42", "vm output mismatch");

    let debug_dir = dir.join("debug");
    fs::create_dir_all(&debug_dir).unwrap();
    let debug_bin = build_native(&path, &debug_dir).expect("debug build");
    let debug = run_native(&debug_bin);
    assert_eq!(debug.2, Some(0), "debug stderr: {}", debug.1);
    assert_eq!(debug.0.trim_end(), "42", "debug output mismatch");

    let release_dir = dir.join("release");
    fs::create_dir_all(&release_dir).unwrap();
    let release_bin = build_native_release(&path, &release_dir).expect("release build");
    let release = run_native(&release_bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(release.2, Some(0), "release stderr: {}", release.1);
    assert_eq!(release.0.trim_end(), "42", "release output mismatch");
}

#[test]
fn aggregate_return_chain_outlives_callee_frame() {
    // Stresses the aggregate-return heap-copy discipline: every
    // iteration calls a function that builds an aggregate on the
    // callee's frame; codegen copies it to the heap at return so
    // the pointer outlives the popped frame, and the caller uses
    // both fields of the returned tuple. The just-returned
    // aggregate must stay intact until the caller consumes it.
    let src = "fn pair_of(i: i64) -> (i64, i64) {\n\
                   (i, i * 7)\n\
               }\n\
               fn main() {\n\
                   let mut sum: i64 = 0\n\
                   let mut i: i64 = 0\n\
                   while i < 5000 {\n\
                       let p = pair_of(i)\n\
                       sum += p.0 + p.1\n\
                       i += 1\n\
                   }\n\
                   println!(\"{}\", sum)\n\
               }\n";
    let expected = (0i64..5000).map(|i| i + i * 7).sum::<i64>();
    let dir = fresh_dir("tracing_gc_return_chain");
    let path = write_source(&dir, "tracing_gc_return_chain", src);

    let run = {
        let child = Command::new(gos_bin())
            .arg("run")
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn gos run");
        run_with_timeout(child)
    };
    assert_eq!(
        run.2,
        Some(0),
        "vm: expected clean exit, got {:?}; stderr: {}",
        run.2,
        run.1
    );
    assert_eq!(
        run.0.trim_end(),
        expected.to_string(),
        "vm aggregate-return chain mismatch (rooted-return discipline broken?)"
    );

    let scratch = dir.join("bin");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("build release");
    let out = {
        let child = Command::new(&bin)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn release binary");
        run_with_timeout(child)
    };
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        out.2,
        Some(0),
        "release: expected clean exit, got {:?}; stderr: {}",
        out.2,
        out.1
    );
    assert_eq!(
        out.0.trim_end(),
        expected.to_string(),
        "release aggregate-return chain mismatch"
    );
}
