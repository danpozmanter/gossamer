# Cross-language benchmark suite

This suite is the evidence source for scoped Gossamer performance claims. Each
workload has matched `.gos` and `.go` sources plus an exact `expected.txt`.
Timing samples are accepted only after both implementations produce that output.

Run on Linux with release tools available:

```bash
cargo build --release -p gossamer-cli
./target/release/gos run benchmarks/suite/run.gos --runs 7 \
  --output benchmarks/suite/results/local.json
```

The measured tiers are `vm-no-jit`, `vm-jit`, `llvm-debug`, `llvm-release`, and
`go-release`. Build time is recorded separately from execution. Results follow
`schema/v1.json`; comparisons use median elapsed time, median absolute deviation,
peak RSS, and binary size. Host and toolchain metadata are mandatory.

Checked-in baselines are review artifacts. The runner exits with an error when
`--compare` names a missing baseline, and CI must never create a baseline from a
pull-request run. Update a baseline explicitly after reviewing output and host
metadata:

```bash
./target/release/gos run benchmarks/suite/run.gos --runs 11 \
  --output benchmarks/suite/baselines/linux-x86_64.json
```

Results from different hosts are not directly comparable. A baseline update
must use the same runner image and toolchain family as the prior baseline.
