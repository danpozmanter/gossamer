# Gossamer — Project Style Guide

This file is the authoritative style guide for the Gossamer compiler and
runtime. It overrides any conflicting guidance.

---

## Language

Only idiomatic Rust. Rust edition 2024, MSRV 1.85.

Use standard library features when they fit. Reach for an external crate
only when the standard library does not provide the primitive
(e.g. `codespan-reporting` for diagnostics, `parking_lot` for mutexes,
`thiserror` for library errors, `anyhow` for top-level application code).

## Safety

Strictly no `unsafe` code. No `unsafe` blocks, no `unsafe fn`, no
`unsafe trait` and no `unsafe impl`. The workspace enables
`#![forbid(unsafe_code)]` in every crate and CI rejects any crate that
drops that lint.

If a subsystem genuinely needs `unsafe` (e.g. stack switching for
stackful coroutines in the runtime), that subsystem must be proposed as
an explicit RFC with its own exemption before landing.

## Cyclomatic Complexity

Prefer low cyclomatic complexity in every function.

- One function does one thing. Extract helpers before a function grows
  branchy.
- Clippy's `cognitive_complexity` lint is set to fail at 15. Do not
  silence it; split the function.
- Prefer `match` exhaustiveness, early `return`, and `?` propagation
  over nested `if let` / `match` pyramids.
- Iterators over index loops; `filter_map` / `fold` / `try_fold` over
  hand-rolled accumulation.

## File Organisation

- No file over 2000 lines (hard limit; CI enforces).
- Prefer files under 500 lines. Split by concern, not by line count.
- Module hierarchy mirrors conceptual structure. One type per file for
  significant types; group small closely-related types together.
- Re-exports at the top of `lib.rs` / `mod.rs` only. No glob re-exports
  outside of `prelude` modules.
- Import grouping: `std` → external crates → workspace crates →
  `crate::` → `super::` → `self::`. Blank line between groups.
  `cargo fmt` handles ordering within a group.

## Documentation

Every type and every function carries a concise docstring.

The Rust compiler still uses `///` and `//!` for machine-readable
doc-comments, and rustdoc cares about the distinction. Those forms stay
in the Rust compiler code. Gossamer source (the `.gos` files under
`examples/` and eventually `crates/gossamer-std/`) uses plain `//`
everywhere per SPEC §2.1.

For Rust code in this repo:

- Use `///` immediately above every `fn`, `struct`, `enum`, `trait`,
  `impl` item, and every field of a public struct. One sentence is
  almost always enough; the first line is a summary fragment.
- Use `//!` at the top of every `lib.rs` / `mod.rs` with a one- or
  two-sentence description of the module's responsibility.
- Docstrings describe intent, invariants, preconditions, and return
  value — not the mechanical body.
- No multi-paragraph docstrings unless documenting a public API that a
  consumer genuinely needs the detail for.

```rust
/// Pixel width of `text` at this font's current size, including kerning.
pub fn measure_text(&self, text: &str) -> u32 { ... }

/// Advance `self.cursor` past one Unicode scalar value.
fn bump(&mut self) { ... }
```

## Comments

No inline comments. No `//` comments inside Rust function bodies and no
trailing end-of-line comments.

If a line needs commentary, either:
1. The code is not clear enough — rename, extract, or restructure until
   it is, or
2. The invariant belongs in the function's docstring.

The only permitted non-doc comments are:
- `// TODO(#NNN):` references pointing at a tracked issue (rare; prefer
  opening the issue and leaving no marker).

No `FIXME`, `XXX`, `HACK`, or unreferenced `TODO` comments in committed
code.

## Naming

Follow Rust convention without exception:
- `snake_case` — functions, methods, locals, modules, fields.
- `UpperCamelCase` — types, traits, enums, enum variants.
- `SCREAMING_SNAKE_CASE` — consts and statics.
- Acronyms treated as words: `Utf8`, `HttpClient`, `JsonValue`.

Names are expressive. Prefer a slightly longer, unambiguous name over a
short, cryptic one.

- Locals: `parser_state`, not `ps`. `token_index`, not `i`. `source`,
  not `s`.
- Iterator bindings may stay short (`for token in tokens`) when the
  enclosing scope is small and the type is obvious from context.
- Booleans read as predicates: `is_empty`, `has_suffix`, `should_retry`.
- Functions are verbs: `parse_item`, `resolve_name`. Types are nouns:
  `Parser`, `TokenStream`.
- Avoid abbreviation except for universally-understood forms (`cfg`,
  `ctx`, `src`, `dst`, `fmt`).

## Error Handling

- `thiserror` for library-facing error enums.
- `anyhow::Result` only at top-level entrypoints (CLI `main`, xtask).
- `?` over `match` for simple propagation.
- No `.unwrap()` or `.expect()` outside of tests. If a value is
  provably non-`None` / non-`Err` at a point where static analysis
  cannot see it, restructure so the impossibility is expressed in the
  type system. No `unreachable!` without a docstring on the enclosing
  function explaining the invariant.

## Types and Structure

- Concrete types over `Box<dyn Trait>` unless dynamic dispatch is
  genuinely required.
- `Arc<parking_lot::Mutex<T>>` only at thread boundaries. Within a
  single thread, plain ownership or `Rc<RefCell<T>>`.
- `parking_lot::Mutex` instead of `std::sync::Mutex` — no poisoning,
  better performance.
- Derive `Debug`, `Clone`, `PartialEq`, `Eq`, `Hash` when they are
  meaningful and cheap. Derive `Default` for types with a sensible
  zero-value.
- Avoid premature lifetime annotations. Restructure to avoid them
  where possible; reach for them only when they express the real
  ownership story of the API.

## Formatting

- `cargo fmt` with the workspace `rustfmt.toml` (line width 100,
  edition 2024). No manual formatting debates; CI enforces.
- Trailing commas in multi-line aggregates.
- One statement per line. No semicolon-separated compound statements.

## Clippy

`cargo clippy --workspace --all-targets -- -D warnings` must pass on
every commit. CI enforces.

No `#[allow(...)]` attributes without a `/// Why:` docstring on the
enclosing item explaining why the lint is wrong for this specific case.
Prefer fixing the code.

Clippy lint groups enabled at workspace level:
- `clippy::all` = deny
- `clippy::pedantic` = warn (case-by-case allow with justification)

`clippy::nursery` and `clippy::cargo` are not enabled by default because
their unstable lints churn and frequently produce false positives. A
specific nursery lint may be opted in at workspace level if it catches a
real class of bug.

## Tests

- Unit tests in a `#[cfg(test)] mod tests { }` at the bottom of the
  file they test.
- Integration tests in each crate's `tests/` directory.
- Test functions name the scenario:
  `fn lexer_emits_invalid_on_stray_backtick()`, not `fn test_1()`.
- Test the public interface; do not reach into private implementation
  details. Exceptions (for regression coverage of a specific
  intermediate computation) must docstring why.
- Snapshot tests via `insta`. Every new lexer / parser feature adds a
  `.gos` input and a snapshot output.

## Unsafe Alternatives

Where performance pressure would historically justify `unsafe`:

- SIMD → `std::simd` (stable) or `wide` crate.
- Uninitialized memory → `MaybeUninit` via safe wrappers, or
  `bytemuck::Zeroable`.
- Raw pointers → indices into a `Vec`-backed arena (our HIR/MIR already
  use this pattern).

## No Emojis

No emojis anywhere — source code, comments, doc comments, commit
messages, CHANGELOG, error messages, PR descriptions, nothing.
