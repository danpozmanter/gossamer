# Combinator boundary cost

Three ways to reach the same arithmetic over a `Vec<i64>`: a `fold`
through the runtime shim, a `map` through it, and a hand-written loop
that never crosses it. The spread between the loop and the two
combinators is what the `gos_rt_*` boundary costs.

The shim is opaque object code to the optimiser: it never inlines, and
where a callback is involved it becomes an indirect call per element
that blocks vectorisation.

Cross-language LTO does not remove that cost. Measured against an
out-of-tree probe it was worth 2%, and the reason is structural rather
than a matter of link-time visibility: `ffi_entry!`'s `catch_unwind`
landing pad blocks inlining and is load-bearing as the FFI panic
boundary, and `push_mapped` branches per element on the element width
and blocking mode read from the runtime vec header, so the dispatch is
data-dependent and no optimiser can fold it. Guard-free, the callback
still did not devirtualise and the loop still did not vectorise. No
`GOS_LTO`-style switch exists in this tree.

```bash
cargo build --release --bin gos
./target/release/gos run benchmarks/combinators/run.gos --runs 3
```

`--elements N` sets the sequence length (default 2,000,000, the count
`baseline.tsv` is stated at) and `--gos PATH` names the toolchain to
measure.
