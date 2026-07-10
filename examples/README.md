# Gossamer examples

Canonical starter programs - each one covers a single topic a new
user will reach for in their first hour with the language. Every
file passes `gos check`, and the set is exercised by the CLI
integration tests across all three tiers (`gos run`, the in-process
JIT, and the `gos build` native binary).

| File | Topic | Status |
|------|-------|--------|
| `hello_world.gos` | First program; `println!` | runs |
| `input.gos` | Basic interactive input with `std::io` | runs |
| `web_server.gos` | HTTP/1.1 routed server over `std::http` | runs |
| `cli_args.gos` | Command-line argument parsing (`std::flag`) | runs |
| `file_io.gos` | File read / write - text + JSON | runs |
| `json_structs.gos` | One-line strict JSON via auto-derived `to_json::<Type>` / `from_json::<Type>` | runs |
| `http_client.gos` | HTTP client / REST call | runs (pair with `web_server.gos`) |
| `data_structures.gos` | Lists, maps, sets from `std::collections` | runs |
| `control_flow.gos` | Loops + conditionals + match (pure syntax) | runs |
| `errors.gos` | `Result<T, E>` + `?` + `std::errors::wrap` | runs |
| `concurrency.gos` | Goroutines + channels - producer / consumer | runs (`gos run` and `gos build`) |
| `go_spawn.gos` | Goroutines without channels - fan-out sketch | runs and builds natively |
| `function_piping.gos` | `|>` forward-pipe operator tour | runs |
| `grep.gos` | Simple Unix-style CLI tool | runs (reads stdin) |
| `testing.gos` | `#[test]` harness + `std::testing` | runs via `gos test` |
| `projects/web_service/` | Full project layout (`project.toml` + `src/`) - multi-endpoint HTTP service with unit tests | `cd` in and run `gos test` |

## Running

```sh
gos run examples/hello_world.gos
gos run examples/input.gos
gos run examples/web_server.gos &
curl 'http://localhost:8080/echo?msg=hi'
gos test examples/testing.gos

cd examples/projects/web_service
gos test           # no args - walks up to project.toml, scans src/
gos run src/main.gos
```

## Conventions

- File-level docstrings use `/* ... */` block comments. Block
  comments do not nest - the first `*/` closes the comment.
- Inline comments use `//`.
- Formatted output goes through the six macros `format!`,
  `println!`, `print!`, `eprintln!`, `eprint!`, and `panic!` -
  one allocation per render, no `+` chains.
- No user-defined macros. `name!(...)` on an unrecognised name
  is a parse error.
- Ordinary double-quoted string literals span multi-line
  without extra syntax.
