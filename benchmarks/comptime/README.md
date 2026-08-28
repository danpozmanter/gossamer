# Comptime fold cost

Comptime evaluates on the bytecode VM while the compiler is running, so
a regression there reaches users as "builds got slower" rather than as a
failing test. These fixtures give that path a number.

Each fixture is one fold shape: string assembly, collection churn, a
numeric loop, all three at once, and a control with the same constants
written as the literals they fold to. The control is what the other four
are read against, and it deliberately never spells the word the fold's
fast path looks for - finding it anywhere in a file, comments included,
is what makes the toolchain run the evaluation front end.

```bash
cargo build --release --bin gos
./target/release/gos run benchmarks/comptime/run.gos --runs 3
```

The runner fails when any fixture is more than 10% (or 20 ms) slower
than `baseline.tsv`, and refuses a timing from a run whose output is not
the value the fold is supposed to produce. `--no-build` measures `gos
check` alone, which is the fold with nothing after it.
