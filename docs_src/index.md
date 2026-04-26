<div class="landing-hero" markdown="1">

<img src="img/GossamerLogo.png" alt="Gossamer logo" class="landing-logo" />

# Gossamer

A garbage-collected, goroutine-powered, fast-compiling systems
language with Rust-flavoured syntax and Go-shaped runtime.

<p class="landing-ctas">
  <a class="landing-cta" href="install/">Get started &rarr;</a>
  <a class="landing-cta-secondary" href="syntax/">Read the docs</a>
</p>

</div>

---

## Read left-to-right with `|>`

```gossamer
fn double(x: i64) -> i64 { x * 2 }
fn add(a: i64, b: i64) -> i64 { a + b }
fn clamp(lo: i64, hi: i64, x: i64) -> i64 {
    if x < lo { lo } else if x > hi { hi } else { x }
}

fn main() {
    let n = 3i64
        |> double
        |> add(10i64)
        |> clamp(0i64, 100i64)
    println("answer:", n)
}
```

`x |> f(a, b)` desugars to `f(a, b, x)` — left-associative, low
precedence, no parentheses needed in a chain.

## Spin up a web server

```gossamer
use std::http

struct App { }

impl http::Handler for App {
    fn serve(&self, request: http::Request) -> Result<http::Response, http::Error> {
        Ok(http::Response::text(200, "hello, gossamer".to_string()))
    }
}

fn main() -> Result<(), http::Error> {
    http::serve("0.0.0.0:8080".to_string(), App { })
}
```

`http::serve` runs the handler on a cooperative-scheduler goroutine
pool; `gos run` is enough to bring it up.

---

- Source on GitHub: [gossamer-lang/gossamer](https://github.com/gossamer-lang/gossamer)
- Status: pre-1.0.0 — nothing here is stable yet.
