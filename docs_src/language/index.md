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
| [`lang::go`](go.md) | Goroutine spawn, detached. |
| [`lang::cohort`](cohort.md) | Structured concurrency: `cohort { }` owns the goroutines `spawn`ed inside it, joins them on every exit path, and reports the first failure as its `Result`. |
| [`lang::triple_quoted_string`](triple_quoted_string.md) | `"""` string literal whose body is dedented by the indentation it shares with its closing delimiter; `gos fmt` moves the block with the line that opens it. |
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
| [`lang::opaque_nominal_alias`](opaque_nominal_alias.md) | `type Name = new Repr` declares a distinct nominal type over an unchanged runtime representation, erased before lowering so no tier sees one. It inherits equality, ordering, hashing, and formatting - which describe the value both sides share - and nothing else: arithmetic needs the alias's own `impl Add`, and the representation's methods are not in scope. `.into()` converts to and from its own representation; any other pair needs `impl From`. |
| [`lang::slicing`](slicing.md) | A range in index position takes a subsequence: `xs[1..3]`, `xs[..k]`, `xs[k..]`, `xs[..]`, `xs[a..=b]`, over fixed arrays, slices, `Vec`, and `String`. Bounds clamp rather than panic, matching `substring`; a `String` slice takes byte offsets and snaps to codepoint boundaries. |
| [`lang::visibility`](visibility.md) | Three visibilities: private by default (the declaring module and its descendants), `pub(package)` (every module of the declaring package), and `pub` (the package's public API). Declared per item, per method, and per struct field; `pub(crate)` / `pub(super)` / `pub(in path)` are rejected (`GP0038`). |
| [`lang::type_alias`](type_alias.md) | Transparent type alias: `type X = T` (and generic `type Pair<A> = (A, A)`) is interchangeable with its target everywhere; a cyclic alias is rejected (`GT0024`). |
| [`lang::mut_ref_params`](mut_ref_params.md) | Local `&mut` aliases write through; `&mut Vec<T>` / `&mut [T]` parameters write through on every tier. |
| [`lang::unicode_identifiers`](unicode_identifiers.md) | Identifiers follow UAX #31 (matches Rust 2024). |
| [`lang::comptime`](comptime.md) | Zig-style compile-time evaluation: `comptime { ... }` blocks, `comptime fn` calls, and `comptime` parameters run on the bytecode VM during compilation and fold to a literal, so every tier compiles the identical constant. `typeInfo::<T>()` reflects a struct's fields, a tuple struct's positions, or an enum's variants - substituting the arguments for a generic instantiation - and a `for (name, ty) in typeInfo::<T>()` loop unrolls into native per-field code, and `codegen!(...)` splices a `comptime fn`'s `String` back as source. Includes the `regex!` / `sql!` build-time validation macros. |
| [`lang::keyword_arguments`](keyword_arguments.md) | Keyword arguments and constant parameter defaults: a call may name any parameter (`volume(depth = 4, width = 2)`), and a parameter may declare a constant default (`fn volume(width: i64, height: i64 = 2)`) that is spliced into every call omitting it. Positional arguments come first, then names. Both are caller-side spellings rewritten into the callee's declared order before type checking, so the calling convention is unchanged. A name on a method call is matched when every type declaring that method name would rewrite the call identically; when they disagree the call is reported (GR0013) rather than guessed. |
| [`lang::move_keyword`](move_keyword.md) | `move` closure capture keyword - declined permanently (SPEC 17.5). Capture is automatic and the runtime manages ownership, so `move` would annotate a decision the language does not make. |
| [`lang::async_await`](async_await.md) | `async fn` / `.await` - declined permanently (SPEC 17.5). Goroutines and channels cover the same shape without colored functions. |
| [`lang::lifetimes`](lifetimes.md) | Explicit lifetime annotations and a borrow checker - declined permanently (SPEC 17.5). References have implicit lexical lifetimes ending at the closing brace, and the lexical `&mut` check is the intended ceiling. |
