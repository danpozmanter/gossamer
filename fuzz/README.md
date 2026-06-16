# Gossamer fuzz corpora

`cargo-fuzz` targets covering every untrusted-input boundary the
toolchain exposes plus differential execution between tiers:

| Target | What it fuzzes | Seed inputs |
|--------|----------------|-------------|
| `lex` | `gossamer_lex::tokenize` - source-level tokenisation. | 10 |
| `parse` | `gossamer_parse::parse_source_file` - full front end. | 9 |
| `manifest` | `gossamer_pkg::Manifest::parse` - `project.toml`. | 5 |
| `http_request` | `gossamer_std::http::parse_{request,status}_line`. | 7 |
| `typecheck` | full front-end through `resolve_source_file` + `typecheck_source_file`. | 0 (libFuzzer-grown) |
| `mir_lower` | front-end + HIR + `lower_program` + `optimise`; asserts `verify_body` post-pass. | 0 (libFuzzer-grown) |
| `vm_compile` | front-end + HIR + bytecode `Vm::load` (no execution). | 0 (libFuzzer-grown) |
| `resolve` | resolver-only driver; takes `Arbitrary`-grown AST and forces it through `resolve_source_file` without a parse-clean prefix. | 0 |
| `hir_lower` | HIR lowering - drives `gossamer_hir::lower_program` on grammar-generated input bypassing typecheck rejection. | 0 |
| `vm_run` | bytecode VM **execution** - `Vm::run` on grammar-generated programs; surfaces `get_unchecked` UB the validator misses. | 0 |
| `differential` | grammar-generated programs through VM, Cranelift JIT and LLVM AOT; byte-compares stdout, panics on divergence. | 0 |

## Running locally

```sh
cargo install cargo-fuzz           # one-time, nightly toolchain required
rustup toolchain install nightly
cd fuzz
cargo +nightly fuzz run lex        # or: parse / manifest / http_request
```

`cargo fuzz` seeds each target with a corpus directory under
`fuzz/corpus/<target>/`. Add regression inputs by dropping a file
into the directory and committing it; they replay on every run.

The crate is kept out of the workspace so `cargo build --workspace`
does not require the cargo-fuzz tooling to be installed.

## Corpus growth policy

The committed corpus is the *seed* corpus - hand-crafted edge
cases and known-buggy inputs. The plan is:

1. **Seed expansion (this directory).** Hand-curated inputs that
   exercise distinct shapes of valid + invalid input. Aim for
   one file per *category* (numbers, strings, comments,
   operators, etc.) rather than one per *random sample*.
2. **Engine corpus (gitignored).** When you run `cargo fuzz
   run <target> -- -max_total_time=3600`, libFuzzer accumulates
   millions of synthetic inputs in
   `fuzz/corpus/<target>/<libfuzzer-generated>`. Do NOT commit
   those - the engine recreates them within minutes.
3. **Crash regression (committed).** A reduced crashing input
   from libFuzzer goes into the seed corpus with a name that
   describes the failure mode (e.g.
   `lex/crash-unterminated-string-with-bom.gos`).

## CI cadence

`.github/workflows/fuzz.yml` runs each target for **30 seconds**
on every PR and push to `main`, smoke-testing that the seed
corpus still parses without panicking. A `cron: "0 3 * * 0"`
job runs each target for an hour every Sunday and uploads any
new crash inputs as workflow artifacts. Both jobs fail loud on
new crashes; they do not warn.

For deeper local fuzzing:

```sh
cd fuzz
for target in lex parse manifest http_request; do
    cargo +nightly fuzz run $target -- -max_total_time=3600
done
```

Reduce a crashing input with `cargo fuzz tmin <target> <crash>`
and commit the minimised reproducer to the corpus.
