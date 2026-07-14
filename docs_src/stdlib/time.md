# `std::time`

Status: experimental

Wall-clock and monotonic time facilities.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Duration`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `type Duration` | Difference between two `Instant`s. |
| [`Instant`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `type Instant` | Monotonic point-in-time. |
| [`SystemTime`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `type SystemTime` | Wall-clock point-in-time. |
| [`format_rfc3339`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn format_rfc3339(ms: i64) -> Result<String, errors::Error>` | Formats a `SystemTime` in RFC 3339 (`YYYY-MM-DDTHH:MM:SSZ`). |
| [`monotonic_ms`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn monotonic_ms() -> i64` | Monotonic clock reading in milliseconds. |
| [`monotonic_nanos`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn monotonic_nanos() -> i64` | Monotonic clock reading in nanoseconds. |
| [`now`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn now() -> time::Instant` | Returns the current monotonic `Instant`. |
| [`now_ms`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn now_ms() -> i64` | Wall-clock milliseconds since the Unix epoch. |
| [`now_nanos`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn now_nanos() -> i64` | Wall-clock nanoseconds since the Unix epoch. |
| [`parse_rfc3339`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn parse_rfc3339(text: String) -> Result<i64, errors::Error>` | Parses an RFC 3339 timestamp into a `SystemTime`. |
| [`since_ms`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn since_ms(instant: time::Instant) -> i64` | Milliseconds elapsed since an earlier monotonic reading. |
| [`sleep`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn sleep(ms: i64) -> ()` | Suspends the current goroutine for `Duration`. |
| [`unix_ms`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn unix_ms() -> i64` | Current Unix time in milliseconds. |
