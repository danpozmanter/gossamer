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
authors = ["Jane Roe <jane@example.com>"]
license = "Apache-2.0"
# Optional: override the `gos build` output path. Relative paths
# resolve against the manifest directory.
output  = "bin/widget"

[dependencies]
"example.org/lib" = "1.2.3"

[registries]
default = "https://registry.gossamer-lang.org"
```

`gos add example.org/lib@1.2.3` appends the dependency.
`gos remove example.org/lib` drops it. `gos tidy` re-serialises
the file in canonical form.

## Module layout

```
src/
├── main.gos       # binary entry
├── lib.gos        # library root (optional)
├── widget.gos     # submodule `widget`
└── sub/
    └── mod.gos    # submodule `sub`
```

Each `.gos` file is its own module. Declare `pub` on anything
you want visible to dependent crates.

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
runs them through the tree-walker.

## Documentation

```gossamer
/// Pixel width of `text` at this font's current size,
/// including kerning.
pub fn measure_text(&self, text: &str) -> u32 { ... }
```

`gos doc src/lib.gos` prints every item + its `///`
summary. HTML output lands with Stream H polish.

## Publishing

*(planned)* — `gos publish` pushes to the default registry once
the backend lands. Until then, path-based + git-based
dependencies in `project.toml` work end-to-end.
