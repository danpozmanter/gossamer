# Gossamer examples

Canonical starter programs — each one covers a single topic a new
user will reach for in their first hour with the language. Every
file parses cleanly through `gos parse`. The runnable subset is
validated by the CLI integration tests.

| File | Topic | Status |
|------|-------|--------|
| `hello_world.gos` | First program; plain `println` | runs |
| `web_server.gos` | HTTP/1.1 echo server over `std::http` | runs |
| `cli_args.gos` | Command-line argument parsing (`std::flag`) | runs when `os::args` is wired |
| `file_io.gos` | File read / write — text + JSON | runs when `std::fs` + `std::encoding::json` are wired |
| `http_client.gos` | HTTP client / REST call | pairs with `web_server.gos` |
| `data_structures.gos` | Lists, maps, sets from `std::collections` | parses; runs partially |
| `control_flow.gos` | Loops + conditionals + match (pure syntax) | runs |
| `errors.gos` | `Result<T, E>` + `?` + `std::errors::wrap` | parses; runs with stdlib pending |
| `concurrency.gos` | Goroutines + channels — producer / consumer | runs via `gos run` (tree-walker + VM); native codegen for `channel()` pending |
| `go_spawn.gos` | Goroutines without channels — fan-out sketch | runs and builds natively |
| `function_piping.gos` | `|>` forward-pipe operator tour | runs |
| `grep.gos` | Simple Unix-style CLI tool | parses; runs when stdin is wired |
| `testing.gos` | `#[test]` harness + `std::testing` | runs via `gos test` |

## Running

```sh
gos run examples/hello_world.gos
gos run examples/web_server.gos &
curl 'http://localhost:8080/echo?name=jane'
gos test examples/testing.gos
```

## Conventions

- File-level docstrings use `/* ... */` block comments. Block
  comments nest: `/* outer /* inner */ still-outer */` is one
  comment.
- Inline comments use `//`.
- Formatted output goes through the six macros `format!`,
  `println!`, `print!`, `eprintln!`, `eprint!`, and `panic!`.
  They expand at parse time to a single call on the internal
  `__concat` builtin — one allocation per render, no `+` chains.
- No user-defined macros. `name!(...)` on an unrecognised name
  is a parse error.
- Ordinary double-quoted string literals span multi-line
  without extra syntax.
