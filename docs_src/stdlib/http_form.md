# `std::http::form`

Status: experimental

application/x-www-form-urlencoded parser and builder.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_form.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Form`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_form.rs) | `type Form` | Parsed url-encoded body, queryable by field name. |
| [`FormBuilder`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_form.rs) | `type FormBuilder` | Builder for url-encoded request bodies. |
