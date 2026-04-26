# Contributing to Gossamer

Thanks for your interest. Gossamer is pre-1.0.0; the API, syntax,
and tooling are all in flux.

## Before you open a PR

- Read `SPEC.md` (language specification) and `GUIDELINES.md` — the
  project style guide. CI enforces every rule in the style guide. No
  exceptions without a written justification in the PR.

## Local checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --workspace
```

All four must pass before the PR is eligible for review.

## Commit messages

One logical change per commit. Imperative subject line under 72
characters. Body wraps at 72. No emojis.

## Licensing

Contributions are licensed under Apache-2.0. By opening a PR you agree
to license your contribution under those terms.
