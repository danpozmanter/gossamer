# Gossamer language reference

One page per language feature. Source is `crates/gossamer-std/src/manifest/feature_status.rs`; this index is regenerated from `manifest::FEATURE_STATUS` by `gos doc --emit-stdlib`.

| Feature | Summary |
|---|---|
| [`lang::let`](let.md) | Immutable binding. |
| [`lang::let_mut`](let_mut.md) | Mutable bindings can be reassigned and can be the source of `&mut`. |
| [`lang::if`](if.md) | Conditional expression. |
| [`lang::match`](match.md) | Exhaustive pattern match expression. |
| [`lang::if_let`](if_let.md) | Single-variant pattern sugar. |
| [`lang::while_let`](while_let.md) | Loop that drains while a pattern matches. |
| [`lang::for`](for.md) | Iterator-driven loop. |
| [`lang::loop`](loop.md) | Unconditional loop with `break value`. |
| [`lang::break`](break.md) | Exit the innermost loop, optionally with a value. |
| [`lang::continue`](continue.md) | Skip to the next iteration of the innermost loop. |
| [`lang::return`](return.md) | Exit the enclosing function with a value. |
| [`lang::question_mark`](question_mark.md) | Short-circuit Result / Option propagation operator. |
| [`lang::pipe`](pipe.md) | Forward-pipe operator `|>`. |
| [`lang::closure`](closure.md) | Lambda expression `|args| body`. |
| [`lang::fn`](fn.md) | Function declaration. |
| [`lang::struct`](struct.md) | Product type declaration. |
| [`lang::enum`](enum.md) | Sum type declaration with payload-carrying variants. |
| [`lang::trait`](trait.md) | Behaviour interface declaration. |
| [`lang::impl`](impl.md) | Inherent and trait implementation blocks. |
| [`lang::generics`](generics.md) | Type parameters on functions / impls / structs. |
| [`lang::go`](go.md) | Goroutine spawn. |
| [`lang::select`](select.md) | Channel multiplex select expression. |
| [`lang::channel`](channel.md) | Typed channel via `std::sync::channel`. |
| [`lang::weak_references`](weak_references.md) | `Weak<T>` downgrade/upgrade handles. Native collection is thread-local only and the bytecode VM has no cycle collector, so cross-tier cyclic reclamation is not yet a Stable guarantee. |
| [`lang::spawn`](spawn.md) | Goroutine join handle: `spawn(f)` -> `JoinHandle<T>`, `.join()` -> `Result<T, String>`. |
| [`lang::macros`](macros.md) | Built-in macros only - no user-defined macros: the format family (print/println/eprint/eprintln/format/panic), the desugar macros (matches!/todo!/unimplemented!/unreachable!/dbg!), and the build-time regex!/sql!/codegen!. |
| [`lang::doctest`](doctest.md) | Fenced code in `//` doc comments runs under `gos test`. |
| [`lang::cfg`](cfg.md) | Conditional compilation attribute. |
| [`lang::attribute`](attribute.md) | Built-in attributes (`#[cfg]`, `#[test]`, `#[bench]`, `#[derive]`). |
| [`lang::const`](const.md) | Compile-time constant binding. |
| [`lang::static`](static.md) | Module-level mutable or immutable static slot. |
| [`lang::type_alias`](type_alias.md) | Transparent type alias: `type X = T` (and generic `type Pair<A> = (A, A)`) is interchangeable with its target everywhere; a cyclic alias is rejected (`GT0024`). |
| [`lang::mut_ref_params`](mut_ref_params.md) | Local `&mut` aliases write through; `&mut Vec<T>` / `&mut [T]` parameters write through on every tier. |
| [`lang::unicode_identifiers`](unicode_identifiers.md) | Identifiers follow UAX #31 (matches Rust 2024). |
| [`lang::comptime`](comptime.md) | Zig-style compile-time evaluation: `comptime { ... }` blocks, `comptime fn` calls, and `comptime` parameters run on the bytecode VM during compilation and fold to a literal, so every tier compiles the identical constant. `typeInfo::<T>()` reflects a type's fields, a `for (name, ty) in typeInfo::<T>()` loop unrolls into native per-field code, and `codegen!(...)` splices a `comptime fn`'s `String` back as source. Includes the `regex!` / `sql!` build-time validation macros. |
| [`lang::move_keyword`](move_keyword.md) | `move` closure capture keyword - parses, lowers to the same Fn shape as a non-move closure (the runtime manages ownership). |
| [`lang::async_await`](async_await.md) | `async fn` / `.await` - goroutines + channels cover the same shape today. |
| [`lang::lifetimes`](lifetimes.md) | References have implicit lexical lifetimes ending at the closing brace; explicit lifetime annotations are not part of safe Gossamer. |
