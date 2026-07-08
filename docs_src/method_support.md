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
| `s.to_lowercase()` | `String` | Lowercase; Unicode-aware. (`to_lowercase` is not a method.) |
| `s.to_uppercase()` | `String` | Uppercase; Unicode-aware. |
| `s.to_string()` | `String` | No-op clone for `&str`/`String`. |
| `s.clone()` | `String` | |
| `s.as_bytes()` | `&[u8]` | Zero-copy borrow. |
| `s.as_str()` | `&str` | Zero-copy borrow. |
| `s.to_i64()` | `Option<i64>` | Parses the string; `None` on malformed input. |
| `s.to_f64()` | `Option<f64>` | |
| `s.to_bool()` | `Option<bool>` | Accepts `true` / `false`. |
| `s.trim_matches(set)` | `String` | Strips any char in `set` from both ends; `trim_start_matches` / `trim_end_matches` do one end. |
| `s.split_once(sep)` | `Option<(String, String)>` | First occurrence; `rsplit_once` takes the last. |

## Vec

Ranges are plain `Vec<i64>` values - `(2..n)` and `(1..=n)` build the
sequence directly, so every method below chains off a range too:
`(1..=5).filter(|n| n % 2 == 1).sum()`.

| Method | Returns | Notes |
|---|---|---|
| `v.push(item)` | `()` | Amortised O(1). |
| `v.pop()` | `Option<T>` | |
| `v.len()` | `i64` | |
| `v.iter()` | `Iter<T>` | Lazy iterator. |
| `v.filter(pred)` | `[T]` | Elements where `pred` holds. |
| `v.map(f)` | `[U]` | Transform every element. |
| `v.sum()` | `T` | Numeric sum; `0` for empty. |
| `v.min()` / `v.max()` | `Option<T>` | `None` for empty. |
| `v.count(pred)` | `i64` | Elements where `pred` holds. |
| `v.any(pred)` / `v.all(pred)` | `bool` | Short-circuiting. |
| `v.find(pred)` | `Option<T>` | First match; `v.position(pred)` returns its index. |
| `v.fold(init, f)` | `U` | Left fold: `f(acc, x)` per element. |
| `v.max_by_key(f)` / `v.min_by_key(f)` | `Option<T>` | Extremum by derived key. |
| `v.take(n)` | `[T]` | First `n` elements (fewer if short). |
| `v.step_by(n)` | `[T]` | Every `n`-th element, starting at index 0. |
| `v.join(sep)` | `String` | Scalar / `String` elements joined with `sep`. |
| `v.first()` / `v.last()` | `Option<T>` | |
| `v.rev()` | `[T]` | Non-mutating; `v.reverse()` is in-place. |
| `v.contains(&x)` | `bool` | `v.index_of(&x)` returns `Option<i64>`, `v.count_of(&x)` the tally. |
| `v.sort()` / `v.sort_by(cmp)` / `v.sort_by_key(f)` | `()` | In-place; `Reverse(k)` keys give descending order. |
| `v.swap(i, j)` | `()` | |

## HashMap

| Method | Returns | Notes |
|---|---|---|
| `m.insert(k, v)` | `()` | Inserts or overwrites in place; does not return the previous value. |
| `m.get(k)` | `Option<V>` | `None` when the key is absent. |
| `m.get_or(k, default)` | `V` | Value for `k`, or `default` when absent. |
| `m.contains_key(k)` | `bool` | Key-membership test (`m.contains(k)` is an alias). |
| `m.remove(k)` | `()` | Deletes the key in place. Use `HashMap::pop(m, k) -> Option<V>` to recover the removed value. |
| `m.inc(k)` / `m.inc(k, by)` | `()` | Increment an `i64` counter, inserting `0` first if absent. |
| `m.or_insert(k, default)` | `V` | Value for `k`, inserting `default` first when absent; works for aggregate values (structs, tuples) too. |
| `m.len()` | `i64` | |
| `m.iter()` | `[(K, V)]` | `keys()` / `values()` return the halves. |

## BTreeMap

`BTreeMap<i64, i64>` and `BTreeMap<i64, String>` are backed by the
key-sorted `IntMap` machinery (same as `HashMap<i64, _>`), so `iter()`
yields pairs in ascending key order.

| Method | Returns | Notes |
|---|---|---|
| `m.insert(k, v)` | `()` | Inserts or overwrites in place. |
| `m.get(k)` | `Option<V>` | `None` when the key is absent. |
| `m.get_or(k, default)` | `V` | Value for `k`, or `default` when absent. |
| `m.contains(k)` / `m.contains_key(k)` | `bool` | Key-membership test. |
| `m.remove(k)` | `()` | Deletes the key in place. |
| `m.len()` | `i64` | |
| `m.iter()` | `[(K, V)]` | Yields pairs in ascending key order. |

## VecDeque

`VecDeque<i64>` is a double-ended ring buffer; both ends are
constant-time. The pop / peek methods return `Option`.

| Method | Returns | Notes |
|---|---|---|
| `d.push_back(v)` | `()` | Append to the back. |
| `d.push_front(v)` | `()` | Prepend to the front. |
| `d.pop_back()` | `Option<T>` | Remove and return the back element. |
| `d.pop_front()` | `Option<T>` | Remove and return the front element. |
| `d.peek_back()` | `Option<T>` | Back element without removing it. |
| `d.peek_front()` | `Option<T>` | Front element without removing it. |
| `d.len()` | `i64` | |
| `d.is_empty()` | `bool` | |

## Channels

`channel::<T>()` returns `(Sender<T>, Receiver<T>)`. Both halves
share these methods:

| Method | Returns | Notes |
|---|---|---|
| `tx.send(v)` | `()` | Enqueues `v`; a buffered send does not block on a waiting receiver. |
| `rx.recv()` | `Option<T>` | Blocks until a value is available; `Some(v)`, or `None` once the channel is closed and drained. The canonical drain is `while let Some(v) = rx.recv()`. |
| `rx.recv_ctx(&ctx)` | `Option<T>` | Blocks like `recv()` but returns `None` when the supplied [`std::context::Context`](stdlib.md#stdcontext) fires. Goroutine callers observe cancellation immediately via the scheduler unpark path; OS-thread callers within 50ms via a bounded condvar timeout. |
| `rx.try_recv()` | `Option<T>` | Non-blocking; `None` if empty. |
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

`I64Vec::new(len)` - heap-allocated atomic-i64 buffer for
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
   as a `#[unsafe(no_mangle)] pub unsafe extern "C" fn` (Rust-side
   declaration; this is internal runtime ABI, not the Gossamer
   source-level FFI surface - see SPEC.md §12 for the latter).
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

- [`stdlib.md`](stdlib.md) - module index.
- [`stdlib_coverage.md`](stdlib_coverage.md) - auto-generated
  coverage matrix.
- [`codegen_abi.md`](codegen_abi.md) - generic call ABI.
