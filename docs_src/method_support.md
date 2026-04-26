# Method support reference

This page lists every method dispatched by name through the
compiler's MIR table at
[`crates/gossamer-mir/src/lower.rs`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-mir/src/lower.rs).
Methods listed here resolve in `gos run` (interpreter) and in
`gos build [--release]` (compiled).

If a method you expect is not listed, the compiler will emit a
`CallIntrinsic{name:"unsupported"}` MIR node and the codegen
will refuse to emit it. File an issue with the call shape; most
gaps are one-line additions to the dispatch table.

## String

| Method | Returns | Notes |
|---|---|---|
| `s.len()` | `i64` | Byte length, not codepoint count. Use `utf8::rune_count` for code points. |
| `s.trim()` | `String` | ASCII whitespace strip. |
| `s.contains(needle)` | `bool` | Substring search. |
| `s.starts_with(prefix)` | `bool` | |
| `s.ends_with(suffix)` | `bool` | |
| `s.find(needle)` | `Option<i64>` | Byte position of first match. |
| `s.replace(from, to)` | `String` | Replaces every occurrence. |
| `s.split(delim)` | `[String]` | Splits on every delimiter occurrence. |
| `s.to_lowercase()` | `String` | ASCII fast-path; falls back to Unicode. |
| `s.to_uppercase()` | `String` | Same. |
| `s.to_string()` | `String` | No-op clone for `&str`/`String`. |
| `s.clone()` | `String` | |
| `s.as_bytes()` | `&[u8]` | Zero-copy borrow. |
| `s.as_str()` | `&str` | Zero-copy borrow. |

## Vec

| Method | Returns | Notes |
|---|---|---|
| `v.push(item)` | `()` | Amortised O(1). |
| `v.pop()` | `Option<T>` | |
| `v.len()` | `i64` | |
| `v.iter()` | `Iter<T>` | Lazy iterator. |

## HashMap

| Method | Returns | Notes |
|---|---|---|
| `m.insert(k, v)` | `Option<V>` | Returns previous value if present. |
| `m.get(k)` | `Option<&V>` | |
| `m.remove(k)` | `bool` | True if the key was present. |
| `m.len()` | `i64` | |

## Channels

`channel::<T>()` returns `(Sender<T>, Receiver<T>)`. Both halves
share these methods:

| Method | Returns | Notes |
|---|---|---|
| `tx.send(v)` | `()` | Blocks if buffered channel is full. |
| `rx.recv()` | `T` | Blocks until a value is available. |
| `tx.try_send(v)` | `bool` | Non-blocking; false if full. |
| `rx.try_recv()` | `Option<T>` | Non-blocking; None if empty. |
| `tx.close()` / `rx.close()` | `()` | Subsequent send/recv return immediately. |

## Streams (`io::stdout` / `io::stderr` / file handles)

| Method | Returns | Notes |
|---|---|---|
| `out.write(s)` / `out.write_str(s)` | `()` | UTF-8 string write. |
| `out.write_byte(b)` | `()` | Single byte. |
| `out.write_byte_array(arr, len)` | `()` | Bulk write from `[i64; N]` or `[u8; N]`. |
| `out.flush()` | `()` | Force buffer drain. |
| `r.read_line()` | `Option<String>` | Up to next `\n` (excluding it). |
| `r.read_to_string()` | `String` | Reads until EOF. |

## Concurrency primitives

`sync::Mutex<T>::new()`:

| Method | Returns | Notes |
|---|---|---|
| `m.lock()` | `()` | Blocks until acquired. |
| `m.unlock()` | `()` | |

`sync::WaitGroup::new()`:

| Method | Returns | Notes |
|---|---|---|
| `wg.add(n)` | `()` | Bumps counter by n. |
| `wg.done()` | `()` | Decrements; notifies on zero. |
| `wg.wait()` | `()` | Blocks until counter reaches zero. |

`sync::AtomicI64::new(initial)`:

| Method | Returns | Notes |
|---|---|---|
| `a.load()` | `i64` | Relaxed ordering. |
| `a.store(v)` | `()` | Relaxed ordering. |
| `a.fetch_add(n)` | `i64` | Returns previous value. |

`I64Vec::new(len)` — heap-allocated atomic-i64 buffer for
goroutine fan-out:

| Method | Returns | Notes |
|---|---|---|
| `b.set_at(i, v)` | `()` | Lock-free atomic store. |
| `b.get_at(i)` | `i64` | Lock-free atomic load. |
| `b.vec_len()` | `i64` | |
| `b.write_range_to_stdout(off, count)` | `()` | Bulk byte write. |
| `b.write_lines_to_stdout(off, count, line_len)` | `()` | Inserts `\n` every `line_len`. |

## Module-style functions

Functions accessed through `use std::module` paths (not method
calls) are listed in [`stdlib_coverage.md`](stdlib_coverage.md).

## Adding a method to the dispatch table

If you need a method that isn't listed:

1. Add the runtime helper in `crates/gossamer-runtime/src/c_abi.rs`
   under `extern "C"`.
2. Add the dispatch arm in
   `crates/gossamer-mir/src/lower.rs::lower_method_call`.
3. Add the LLVM declaration in
   `crates/gossamer-codegen-llvm/src/emit.rs::RUNTIME_DECLARATIONS`.
4. Add the Cranelift symbol arm in
   `crates/gossamer-codegen-cranelift/src/native.rs`.
5. Register the interpreter builtin in
   `crates/gossamer-interp/src/builtins.rs::install_concurrency_builtins`
   (or the matching install function).
6. Add a small test in `crates/gossamer-codegen-cranelift/tests/`
   that exercises both tiers.

The contract is "every method visible at the language level
resolves at every tier." A method missing from any of (a) the
dispatch table, (b) the LLVM declarations, (c) the interpreter
builtins, is a bug.

## Cross-references

- [`stdlib.md`](stdlib.md) — module index.
- [`stdlib_coverage.md`](stdlib_coverage.md) — auto-generated
  coverage matrix.
- [`codegen_abi.md`](codegen_abi.md) — generic call ABI.
