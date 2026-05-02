# Changelog

## 0.0.1

### Stdlib parity

- `flag` stdlib fully wired in compiled mode. Default values for
  `int`, `float`, `duration`, `string_list`, `short`, `usage` are now
  honoured (previously every non-`string`/`uint`/`bool` flag silently
  zeroed). `parse` accepts the `=` form, short aliases, `--`, and
  `--help` / `-h`. Interp gained matching `float` / `duration` /
  `string_list` / `usage` builtins so both tiers produce identical
  output across every flag method.
- `flag::define(name, [flag::int(...), flag::string(...),
  flag::bool(...)])` (declarative one-shot constructor) now lowers
  to the imperative `flag::Set` builder chain at MIR time.
  Previously interp-only — compiled mode silently returned a
  null-shaped struct so `*flags.<long>` always yielded the
  primitive zero (xkcd-cli's `get_xkcd` reproducer).
- `os::env`, `os::cwd` wired in both tiers. Compiled mode was
  returning `0` for every env var lookup and `0` for `cwd`.
- `fs::list_dir` wired in compiled mode (returns
  `Result<[DirInfo], Error>`).
- `time::Duration::from_secs` / `from_millis` lower in compiled mode.

### Compiler / codegen fixes

- Aggregate construction is now heap-allocated (`calloc`) instead
  of stack-slot. Returning a struct from a method (e.g.
  `Celsius { value: ... }.to_fahrenheit()`) no longer aliases the
  next call's stack slot; `temperature.gos` now matches across
  tiers.
- `loop { ... break <expr> }` captures the break expression's
  value in compiled mode. Previously
  `let x = loop { ... break sq }` returned 0 instead of `sq`.
- `result.map_err(closure)` and `result.map(closure)` dispatch
  correctly when the receiver type is unresolved at HIR time
  (e.g. `text.parse().map_err(...)?`). The closure was being
  built and silently dropped.
- String equality (`s == "literal"`, `s != "literal"`) routes
  through `gos_rt_str_eq`. Previously a pointer-compare that
  silently disagreed with interpreted output whenever the string
  came from a runtime helper rather than a literal-pinned slot.
- Reference deref (`*p` where `p: &i64` / `&f64` / `&bool` /
  `&char`) emits a real load instead of returning the pointer
  unchanged. Affected every iterator pattern that yields scalar
  references.
- `s.as_bytes()` returns a `Vec<i64>` shape (one zero-extended
  byte per slot) instead of a packed `Vec<u8>`. Compiled `bytes[i]`
  indexing now reads the byte's value through
  `gos_rt_vec_get_i64` rather than reading 8 packed buffer
  bytes as a single i64 (`reverse_string.gos` reproducer).
- `<chain>.method().to_string()` dispatches to the right runtime
  formatter (`gos_rt_i64_to_str` / `gos_rt_f64_to_str` /
  identity for strings) when the typechecker leaves the chain's
  HIR type as a `Var(_)`. Previously the identity-copy fallback
  fed an i64 to `gos_rt_str_concat` as a c_char* — segfault.
- Better error messages.
- Actual Error types.

### Test coverage

- `cargo test -p gossamer-cli --test parity --features
  exhaustive_tests --release` walks every example in
  `examples/*.gos` under both tiers and asserts byte-identical
  stdout/stderr/exit code. Two examples (`go_spawn.gos`,
  `list_dir.gos`) are listed in `KNOWN_DIVERGENT_EXAMPLES` with
  explicit root-cause comments — go_spawn requires a
  deterministic scheduler shared between tiers, list_dir
  requires registering `fs::DirInfo` as a stdlib struct in
  `gossamer-types::TyCtxt::register_struct_fields` at
  typechecker startup. Every other example round-trips.
- `crates/gossamer-codegen-cranelift/tests/correct/p51_flag_defaults`
  walks every flag type through interp + Cranelift + LLVM tiers.

## 0.0.0

Initial release. Not production ready.
