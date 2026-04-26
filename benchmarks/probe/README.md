# Codegen probe matrix

One-construct `.gos` programs that exercise individual codegen
features. `check.sh` runs `gos build` on each, classifies the
result (`native` / `failed`), and diffs against `results.txt` —
so the committed baseline doubles as a regression guard while the
Cranelift backend grows.

`results.txt` is the committed baseline. Update it deliberately
whenever a probe's classification changes (that is, whenever a
codegen milestone lands a new capability). The `check.sh` exit code
is the test signal.

## Current coverage (post-L3)

26 / 26 probes go native. Covers every integer and float primitive
op, `if`/`else`/`while`, function call + early return, fixed-size
arrays of scalar and struct elements, `struct { … }` field
read/write, `[Struct; N].field` projected read/write, `println!`
formatted output, booleans, characters, tuples, string literals,
string methods (`.len()`), `for i in range`, capturing closures,
function-pointer locals (`let f = fn; f(x)`), call results into a
struct field projection, float `%` (libc fmod), `[v; N]` repeat
arrays, and round-trip channel send/recv via the linked
`gossamer-runtime` staticlib.
