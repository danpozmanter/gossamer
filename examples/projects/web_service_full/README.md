# web_service_full — Track B joint-validation target

End-to-end HTTP service stack:

- HTTP API with sqlite-backed CRUD
- mTLS for service-to-service auth
- Structured JSON logging via `std::slog`
- Graceful shutdown via `signal::Notifier`
- Run-time templating via `std::html::template`

This service is the load-test target Track A uses to prove out the
netpoller / scheduler integration. Until those land, the service
runs on the thread-pool-backed I/O fallback documented in
`~/dev/contexts/lang/prod_gaps.md`.

## Endpoints

| Method  | Path                    | Notes                              |
|---------|-------------------------|------------------------------------|
| `GET`   | `/health`               | `200 ok`                           |
| `GET`   | `/notes`                | List every note as JSON.           |
| `POST`  | `/notes`                | Create a note from JSON body.      |
| `GET`   | `/notes/<id>`           | Single note by id.                 |
| `DELETE`| `/notes/<id>`           | Delete by id.                      |
| `GET`   | `/notes.html`           | HTML rendering via the template.   |

## Running

```
gos run            # interpreter mode (HTTP only)
gos build --release
GOS_TLS_CERT=server.pem GOS_TLS_KEY=server.key ./web_service_full
```

When `GOS_TLS_CERT` / `GOS_TLS_KEY` / `GOS_TLS_CLIENT_CA` are set,
the server runs over mTLS instead of plain HTTP.

## Testing

```
gos test --parallel 4 --format junit --junit-out report.xml
```

The harness exercises every handler, the JSON encode / decode
round trip, and the html template render.
