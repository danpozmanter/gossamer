# Gossamer fuzz corpora

Four `cargo-fuzz` targets live here. Each targets a parser we expose
to untrusted input:

| Target | What it fuzzes | Seed inputs |
|--------|----------------|-------------|
| `lex` | `gossamer_lex::tokenize` — source-level tokenisation. | 10 |
| `parse` | `gossamer_parse::parse_source_file` — full front end. | 9 |
| `manifest` | `gossamer_pkg::Manifest::parse` — `project.toml`. | 5 |
| `http_request` | `gossamer_std::http::parse_{request,status}_line`. | 7 |

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

The committed corpus is the *seed* corpus — hand-crafted edge
cases and known-buggy inputs. The plan is:

1. **Seed expansion (this directory).** Hand-curated inputs that
   exercise distinct shapes of valid + invalid input. Aim for
   one file per *category* (numbers, strings, comments,
   operators, etc.) rather than one per *random sample*.
2. **Engine corpus (gitignored).** When you run `cargo fuzz
   run <target> -- -max_total_time=3600`, libFuzzer accumulates
   millions of synthetic inputs in
   `fuzz/corpus/<target>/<libfuzzer-generated>`. Do NOT commit
   those — the engine recreates them within minutes.
3. **Crash regression (committed).** A reduced crashing input
   from libFuzzer goes into the seed corpus with a name that
   describes the failure mode (e.g.
   `lex/crash-unterminated-string-with-bom.gos`).

## CI cadence

The CI workflow runs each target for **30 seconds** on every
push to main, smoke-testing that the seed corpus still parses
without panicking. A weekly job runs each target for an
hour and uploads any new crashes.

For deeper local fuzzing:

```sh
cd fuzz
for target in lex parse manifest http_request; do
    cargo +nightly fuzz run $target -- -max_total_time=3600
done
```

Reduce a crashing input with `cargo fuzz tmin <target> <crash>`
and commit the minimised reproducer to the corpus.
