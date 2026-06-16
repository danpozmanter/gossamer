# `std::archive`

Archive readers and writers. Two formats ship in 0.4.0:

- [`std::archive::zip`](#stdarchivezip) - RFC 1952 zip.
- [`std::archive::tar`](#stdarchivetar) - Unix tar (USTAR /
  PAX-aware decode).

## `std::archive::zip`

| Name | Kind | Description |
|---|---|---|
| `Reader` | type | Random-access zip reader (in-memory or file-backed). |
| `Reader::open` | fn | Open a file by path. |
| `Reader::open_bytes` | fn | Parse zip bytes. |
| `Reader::names` | fn | Returns the list of entry names. |
| `Reader::read_file` | fn | Reads a single entry by name. |
| `Writer` | type | Streaming zip writer. |
| `Writer::create` | fn | Create a new archive at path. |
| `Writer::add_file` | fn | Add a named entry with raw bytes. |
| `Writer::finish` | fn | Finalise the archive (writes central directory). |
| `Error` | enum | `Io`, `Format`, `NotFound`, `Unsupported`. |

## `std::archive::tar`

| Name | Kind | Description |
|---|---|---|
| `Reader` | type | Iterating tar reader over any `Read` source. |
| `Reader::open` | fn | Open a file by path. |
| `Reader::entries` | fn | Yields one `Entry` per archive member. |
| `Entry` | type | One tar entry - has `path`, `kind`, `size`, `mode`, and a `read` method to extract the body. |
| `Writer` | type | Streaming tar writer. |
| `Writer::create` | fn | Create a new archive. |
| `Writer::add_file_from_path` | fn | Add a file from disk by path. |
| `Writer::add_bytes` | fn | Add a named entry from in-memory bytes. |
| `Writer::finish` | fn | Finalise the archive (zero blocks). |
| `Error` | enum | `Io`, `Format`, `Unsupported`. |
