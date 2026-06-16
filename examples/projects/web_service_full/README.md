# web_service_full

A complete, runnable HTTP service that exercises the production-stack
shape end to end and behaves identically on every tier (bytecode VM,
Cranelift JIT, LLVM AOT):

- HTTP routing with path parameters via `std::http::router`
- A notes API in CRUD shape (`GET`/`POST`/`DELETE`)
- Structured JSON logging via `std::slog`
- Signal-driven graceful shutdown via `std::os::signal`
- Server-side HTML rendering (HTML-escaped) for the browser view

The notes "store" is an in-memory seeded dataset. The language ships
no in-the-box SQL driver — a driver is wired per project through the
`[rust-bindings]` mechanism — so this example keeps the data in
process to stay self-contained and runnable everywhere. Reads come
from the seed; create/delete return the operation result the way a
REST front end does.

## Endpoints

| Method   | Path             | Notes                              |
|----------|------------------|------------------------------------|
| `GET`    | `/health`        | `200 ok`                           |
| `GET`    | `/notes`         | List every seeded note as JSON.    |
| `POST`   | `/notes`         | Create a note from a JSON body.    |
| `GET`    | `/notes/{id}`    | Single note by id (`200` / `404`). |
| `DELETE` | `/notes/{id}`    | Delete by id (`204` / `400`).      |
| `GET`    | `/notes.html`    | HTML listing (escaped).            |

The `{id}` segment is a router path parameter, read back in the
handler with `r.path_int("id") -> Option<i64>`.

## Running

```
gos run                    # bytecode VM
gos build --release && ./web_service_full

curl -i http://localhost:8080/health
curl -i http://localhost:8080/notes
curl -i -X POST http://localhost:8080/notes -d '{"body":"hello"}'
curl -i http://localhost:8080/notes/2
curl -i -X DELETE http://localhost:8080/notes/2
curl -i http://localhost:8080/notes.html
```

Structured logs are written to stderr as one JSON object per line.
Press Ctrl-C (or send `SIGTERM`) to trigger the graceful-shutdown
coordinator, which logs and exits cleanly.

## Testing

```
gos test
```

The unit tests cover the seed lookup, the JSON encoder, the HTML
escaper, and the request-body parser without binding a socket.
