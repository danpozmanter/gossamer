# feature-testing-examples

Twenty parity stress examples designed to surface gaps between Gossamer's
execution tiers: bytecode VM (with Cranelift JIT), Cranelift AOT debug,
and LLVM AOT release.

## Quick start

```
gos check *.gos              # type-check everything
gos run example.gos              # run one example on the VM
gos run --no-jit example.gos     # run one example on the VM without JIT
gos test .                   # run #[test] functions
gos build example.gos        # compile one non-channel example to native
```

## Examples

| File | What it tests | Known gaps |
|---|---|---|
| `variable_shadowing_ladder.gos` | Rebinding through blocks, `if`, `match`, loops | Works on all tiers |
| `integer_overflow_edges.gos` | `i64`/`u64` limits, casts, comparisons | Works on all tiers |
| `float_cast_drift.gos` | Int/float mixing with `as` | Works on all tiers |
| `pattern_match_exhaustiveness.gos` | Nested enums, guards, `if let` | Works on all tiers |
| `option_unwrap_chain.gos` | `Option`/`Result` chaining with `?` | Works on all tiers |
| `closure_capture_mutation.gos` | Immutable capture + higher-order functions | Works on all tiers |
| `pipe_operator_precedence.gos` | Long `|>` chains with arithmetic | Works on all tiers (the step names its slot) |
| `recursive_enum_walk.gos` | Recursive `enum` + `Box` list/tree | Works on all tiers |
| `tuple_destructuring_loop.gos` | Destructuring in `for`, `let`, `while let`, rest `..` | Works on all tiers |
| `string_concatenation_stress.gos` | `+`, `+=`, `format`, `println` | Works on all tiers |
| `hashmap_counter_race.gos` | `HashMap.inc`, `or_insert`, repeated updates, iteration | Works on all tiers |
| `channel_fan_in.gos` | Multiple goroutines into one channel | Works on VM; `gos build` not wired |
| `select_default_timing.gos` | Channel polling loop (fallback for `select`) | Works on VM; `gos build` not wired |
| `mutex_vs_channel_counter.gos` | Shared counter via `Mutex` vs channels | Works on all tiers |
| `sort_with_closure.gos` | `arr.sort_by` with custom comparator | Works on all tiers |
| `error_chain_inspection.gos` | `errors.wrap`, `chain`, `is` | Works on all tiers |
| `reference_alias_mutation.gos` | `&T` and `&mut T` through helpers | Works on all tiers |
| `array_bounds_probe.gos` | Valid/invalid indices with `i64` | Works on all tiers |
| `method_dispatch_collision.gos` | Same method names across `impl` blocks | Works on all tiers |
| `doc_test_vs_unit_test_drift.gos` | Doc-test vs `#[test]` parity | Works on all tiers |

## Full gap analysis

See `~/dev/contexts/lang/observed_gaps.md` for a detailed breakdown with
output diffs, work-arounds, and a feature support matrix across all tiers.
