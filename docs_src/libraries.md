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

`gos add example.org/lib@1.2.3` appends the dependency.
`gos remove example.org/lib` drops it. `gos tidy` re-serialises
the file in canonical form.

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
subdirectories, recursively. Each `.gos` file is its own module;
declare `pub` on anything you want visible to other modules or to
dependent packages.

The entry (or library root) declares the top-level modules with
`mod NAME;`:

```gossamer
// src/main.gos
mod widget;
mod sub;

fn main() {
    println!("{}", widget::greet(&"world"))
    println!("{}", sub::ping())
}
```

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
`self::child::item` (a child of the current module). `gos run`,
`gos build`, and `gos check` all assemble the package the same way, so
a directory argument (`gos run my_project`) or `gos check src/` checks
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
src/lib.gos` prints every item plus that summary block. HTML
output lands with Stream H polish.

## Publishing

*(planned)* - `gos publish` pushes to the default registry once
the backend lands. Until then, path-based + git-based
dependencies in `project.toml` work end-to-end.
