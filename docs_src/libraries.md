# Writing libraries

## Scaffolding a project

```sh
gos new example.com/widget --path widget
cd widget
```

You get:

```
widget/
├── project.toml
└── src/
    └── main.gos
```

## The `project.toml` manifest

```toml
[project]
id      = "example.com/widget"
version = "0.1.0"
authors = ["Leslie Tungsten <ltungsten@example.com>"]
license = "Apache-2.0"

[dependencies]
"example.org/lib" = "1.2.3"

[registries]
default = "https://registry.gossamer-lang.org"

# Required before the first registry fetch for a package. The registry
# cannot establish this binding by advertising a key in its index.
[trusted-publishers]
"example.org/lib" = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

# Optional: explicit binary targets. Without this section, the
# default is one binary named after the project id whose entry
# point is `src/main.gos`.
[[bin]]
name = "widget"
path = "src/main.gos"

# Optional: a library target alongside / in place of a binary.
# Without this section, presence of `src/lib.gos` is enough to
# build the library by convention.
[lib]
name = "widget"
path = "src/lib.gos"
```

A dependency is keyed by the project id it publishes under, or - when its
source names its own identity, as a `git`, `path`, or `tarball` entry does -
by the module name source imports it as:

```toml
[dependencies]
pgsql_gos = { git = "https://github.com/danpozmanter/pgsql-gos" }
```

A package name may carry `-`, which no identifier may, so its module name is
the final path segment with each `-` replaced by `_`. Every import spelling
that names that module reaches the package: `use pgsql_gos`,
`use pgsql_gos::greet`, `use pgsql_gos::{greet}`, `use pgsql_gos as pg`, and
`use "github.com/danpozmanter/pgsql-gos"`. A `use pgsql-gos` is rejected
(`GP0040`) - `-` is subtraction, never part of an identifier - and two
dependencies reaching source under one module name are rejected (`GR0019`),
which an explicit alias or a distinct key resolves.

A git source is versioned by the reference it is checked out at: `tag`,
`branch`, or `rev`, defaulting to `main`. The resolved reference is written to
`project.lock`, so a build repeats the same checkout.

```toml
[dependencies]
pgsql_gos = { git = "https://github.com/danpozmanter/pgsql-gos", tag = "v1.2.3" }
```

A `version` range belongs to a registry dependency, which resolves within it;
writing one beside `git` is rejected rather than silently ignored.

`gos add example.org/lib@1.2.3` appends the dependency.
`gos remove example.org/lib` drops it. `gos update` refreshes selected versions
within declared ranges. `gos tidy` parses project sources, removes direct
project dependencies that are not imported, and writes canonical ordering.
Rust binding dependencies are retained independently.

The default convention is still: `src/main.gos` ⇒ binary,
`src/lib.gos` ⇒ library, project id ⇒ output name. The
`[[bin]]` / `[lib]` sections let you override the entry-point
path, rename the output, or ship multiple binaries from one
project.

## Selecting the entry file

For a single-binary project, the optional `[project] entry` key names
the entry source directly, overriding convention-based resolution:

```toml
[project]
id      = "example.com/widget"
version = "0.1.0"
entry   = "src/app.gos"
```

The path is relative to the manifest directory. The resolved entry is
the only file allowed to carry [top-level
statements](language/top_level_statements.md); sibling and library
modules contain items only.

## Module layout

A package spans files and directories:

```
src/
├── main.gos       # binary entry  (default; override via [[bin]].path)
├── lib.gos        # library root  (default; override via [lib].path)
├── widget.gos     # submodule `widget`
└── sub/
    ├── mod.gos    # submodule `sub`
    └── deep/
        └── mod.gos  # submodule `sub::deep`
```

A sibling `src/<name>.gos` is the module `name`. A subdirectory is a
module when it carries a `mod.gos` root (`src/<dir>/mod.gos` is the
module `dir`), and it may nest its own sibling files and
subdirectories, recursively, to any depth. Each `.gos` file is its own
module; declare `pub` on anything you want visible to other modules or
to dependent packages.

The layout declares the modules, so the entry needs no `mod NAME;` line.
A module's items are not in scope on their own: name them through a path
or bring them in with `use`.

```gossamer
// src/main.gos
use widget::greet

fn main() {
    println!("{}", greet(&"world"))
    println!("{}", sub::ping())
}
```

Writing a bare `greet(..)` without the import reports `GR0011`, which
names the declaring module and the exact `use` line to add. A type
belongs to the module that declares it, so two modules may each declare
a `Config` without the two colliding.

```gossamer
// src/widget.gos
pub fn greet(name: &String) -> String {
    // Reach another module from the package root with `crate::`,
    // or one level up with `super::`.
    crate::sub::banner() + ", " + name
}
```

A module reaches another by a navigation path: `crate::other::item`
(rooted at the package), `super::other::item` (one level up), or
`self::child::item` (a child of the current module). `gos`,
`gos build`, and `gos check` all assemble the package the same way, so
a directory argument (`gos my_project`) or `gos check src/` checks
the whole package as one unit.

## Unit + integration tests

```gossamer
// inside src/widget.gos
pub fn add(a: i64, b: i64) -> i64 { a + b }

#[cfg(test)]
mod tests {
    #[test]
    fn add_adds() {
        let total = super::add(2, 3)
        assert(total == 5)
    }
}
```

Integration tests live under `tests/`. `gos test src/lib.gos`
runs them on the register-based bytecode VM.

## Documentation

```gossamer
// Pixel width of `text` at this font's current size,
// including kerning.
pub fn measure_text(&self, text: &str) -> u32 { ... }
```

Gossamer uses one comment form: `//` for line comments and
`/* ... */` for block comments. There is no separate `///` /
`//!` doc-comment syntax - a run of `//` lines directly above
an item (no blank line between) is its documentation, and a
run at the top of a file is the module's. `gos doc
src/lib.gos` prints every item plus that summary block;
`gos doc --html <path> src/lib.gos` writes an HTML page instead.

## Foreign code (`[rust-bindings]`)

To call native (Rust) code, declare a binding crate under
`[rust-bindings]` in `project.toml`. The crate depends on
`gossamer-binding` and registers its entry points with
`register_module!`; the toolchain builds it into a per-project runner
and links it into the binary (or interpreter), after which the bound
functions are `use`-able from `.gos` source like any other module.

```toml
# project.toml
[rust-bindings]
echo-binding = { path = "echo-binding" }
```

```rust
// echo-binding/src/lib.rs
use gossamer_binding::register_module;
register_module!("echo", {
    fn shout(s: String) -> String { s.to_uppercase() }
});
```

```gossamer
use echo::shout
fn main() { println!("{}", shout("hello")) }
```

Values cross the boundary through the typed `gossamer-binding` ABI
(integers, floats, strings, tuples, vectors, `Option` / `Result`,
opaque handles, byte buffers, callbacks); a panic in a binding is
caught and returned as `Result::Err`. This is the **only** FFI surface
- a source-level `extern "C"` item form is rejected (`GP0016`) and the
`extern` keyword stays reserved. Calls run end-to-end under `gos`
and link into `gos build` binaries; direct compiled-tier dispatch into
binding thunks lands incrementally as more binding shapes are wired.
See the SPEC (section 12 in the repository root),
`crates/gossamer-binding/ABI_0_4.md`, and the
`example-external-libraries/` projects for full detail.

## Publishing

`gos publish` packs the project, signs the tarball (Ed25519), and
uploads it to the registry; `--dry-run` packs and signs without
uploading. `gos yank`, `gos login` / `gos logout`, and `gos owner`
round out the registry workflow, with dependency tarballs sha256-pinned
in `project.lock`. A registry package must also have a publisher key pinned
in `project.lock` or explicitly bound in `[trusted-publishers]` before its
first fetch; keys advertised only by a registry index are not trusted.
Path-based and git-based dependencies in
`project.toml` also work end-to-end.
