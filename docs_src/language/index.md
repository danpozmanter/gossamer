# Gossamer language reference

One page per language feature. Source is `crates/gossamer-std/src/manifest/feature_status.rs`; this index is regenerated from `manifest::FEATURE_STATUS` by `gos doc --emit-stdlib`.

| Feature | Status | Summary |
|---|---|---|
| [`lang::let`](let.md) | shipped | Immutable binding. |
| [`lang::let_mut`](let_mut.md) | shipped | Mutable binding. |
| [`lang::if`](if.md) | shipped | Conditional expression. |
| [`lang::match`](match.md) | shipped | Exhaustive pattern match expression. |
| [`lang::if_let`](if_let.md) | shipped | Single-variant pattern sugar. |
| [`lang::while_let`](while_let.md) | shipped | Loop that drains while a pattern matches. |
| [`lang::for`](for.md) | shipped | Iterator-driven loop. |
| [`lang::loop`](loop.md) | shipped | Unconditional loop with `break value`. |
| [`lang::break`](break.md) | shipped | Exit the innermost loop, optionally with a value. |
| [`lang::continue`](continue.md) | shipped | Skip to the next iteration of the innermost loop. |
| [`lang::return`](return.md) | shipped | Exit the enclosing function with a value. |
| [`lang::question_mark`](question_mark.md) | shipped | Short-circuit Result / Option propagation operator. |
| [`lang::pipe`](pipe.md) | shipped | Forward-pipe operator `\|>`. |
| [`lang::closure`](closure.md) | shipped | Lambda expression `\|args\| body`. |
| [`lang::fn`](fn.md) | shipped | Function declaration. |
| [`lang::struct`](struct.md) | shipped | Product type declaration. |
| [`lang::enum`](enum.md) | shipped | Sum type declaration with payload-carrying variants. |
| [`lang::trait`](trait.md) | shipped | Behaviour interface declaration. |
| [`lang::impl`](impl.md) | shipped | Inherent and trait implementation blocks. |
| [`lang::generics`](generics.md) | shipped | Type parameters on functions / impls / structs. |
| [`lang::go`](go.md) | shipped | Goroutine spawn. |
| [`lang::select`](select.md) | shipped | Channel multiplex select expression. |
| [`lang::channel`](channel.md) | shipped | Typed channel via `std::sync::channel`. |
| [`lang::spawn`](spawn.md) | shipped | Goroutine join handle: `spawn(f)` -> `JoinHandle<T>`, `.join()` -> `Result<T, String>`. |
| [`lang::macros`](macros.md) | shipped | Six built-in format macros (print/println/eprint/eprintln/format/panic). |
| [`lang::doctest`](doctest.md) | shipped | Fenced code in `//` doc comments runs under `gos test`. |
| [`lang::cfg`](cfg.md) | shipped | Conditional compilation attribute. |
| [`lang::attribute`](attribute.md) | shipped | Built-in attributes (`#[cfg]`, `#[test]`, `#[bench]`, `#[derive]`). |
| [`lang::const`](const.md) | shipped | Compile-time constant binding. |
| [`lang::static`](static.md) | shipped | Module-level mutable or immutable static slot. |
| [`lang::mut_ref_params`](mut_ref_params.md) | shipped | `&mut Vec<T>` / `&mut [T]` parameters write through to the caller's storage on every tier. |
| [`lang::unicode_identifiers`](unicode_identifiers.md) | shipped | Identifiers follow UAX #31 (matches Rust 2024). |
| [`lang::move_keyword`](move_keyword.md) | planned | `move` closure capture keyword - parses, lowers to the same Fn shape as a non-move closure (the runtime manages ownership). |
| [`lang::async_await`](async_await.md) | planned | `async fn` / `.await` - goroutines + channels cover the same shape today. |
| [`lang::lifetimes`](lifetimes.md) | planned | Explicit lifetime annotations - not needed under the current memory model; tracked in case a borrow-checker mode lands. |

