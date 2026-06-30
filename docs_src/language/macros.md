# `lang::macros`

Status: shipped

Built-in macros only - no user-defined macros: the format family (print/println/eprint/eprintln/format/panic), the desugar macros (matches!/todo!/unimplemented!/unreachable!/dbg!), and the build-time regex!/sql!/codegen!.
<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

The macro set is fixed. Every `name!(...)` outside this list is a parse
error (`GP0001`) - there are no user-defined macros.

## Format macros

`format!`, `println!`, `print!`, `eprintln!`, `eprint!`, and `panic!`
take a literal template plus arguments. Placeholders are positional `{}`,
named `{ident}` (a binding in scope), and `{:spec}` / `{name:spec}` with
Rust's width / fill / align / radix / precision grammar:

```gossamer
let name = "jane"
println!("hello, {name}")          // named capture
println!("[{:>8.2}]", 3.14159)     // [    3.14]
let msg = format!("{} / {}", 1, 2) // returns a String
```

A placeholder whose name is an expression (`{age + 1}`) is a parse error
(`GP0021`).

## Desugar macros

These expand at parse time into ordinary constructs, so they lower
uniformly on every tier:

- `matches!(expr, pat)` - boolean: does `expr` match the pattern.
- `todo!` / `unimplemented!` / `unreachable!` - `panic!` with a fixed (or
  supplied) message.
- `dbg!(expr)` - prints `expr` with `{:?}` to stderr and yields its value.

```gossamer
if matches!(n, 1..=9) { /* single digit */ }
let x = dbg!(compute())   // logs the value, returns it
```

## Build-time macros

`regex!("…")` and `sql!("…")` validate their literal argument at compile
time and fold to the validated string; `codegen!(...)` splices a `comptime
fn`'s `String` result back as source. See [comptime](comptime.md).
