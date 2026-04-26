# web_service example project

A small multi-endpoint HTTP service laid out as a real Gossamer
project (with a `project.toml`). Demonstrates:

- An `http::Handler` impl that dispatches on `request.path()`.
- Pure render helpers for each route — directly unit-testable
  without spinning up a real socket.
- `#[cfg(test)] mod tests` exercising every endpoint helper.

## Layout

```
web_service/
├── project.toml
├── README.md
└── src/
    └── main.gos     server bootstrap, routes, render helpers, tests
```

## Run

```sh
cd examples/projects/web_service
gos run src/main.gos
curl http://localhost:8080/health
curl http://localhost:8080/users
curl 'http://localhost:8080/echo?msg=hi'
```

## Test

`gos test` with no arguments walks up to the nearest
`project.toml` and discovers every `.gos` file under `src/`:

```sh
cd examples/projects/web_service
gos test
```
