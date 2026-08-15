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
| [`CivilTime`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `type CivilTime` | Calendar fields interpreted with an explicit location. |
| [`CivilResolution`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `enum CivilResolution { Unique(i64), Gap, Fold(i64, i64) }` | Explicit civil-to-timeline resolution. |
| [`Location`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `type Location` | Immutable UTC, fixed-offset, or IANA time-zone location. |
| [`add_date`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn add_date(unix_ms: i64, location: Location, years: i64, months: i64, days: i64) -> Result<i64, errors::Error>` | Calendar addition that rejects gap and fold results. |
| [`format_in`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn format_in(layout: String, unix_ms: i64, location: Location) -> Result<String, errors::Error>` | Formats an instant in an explicit location. |
| [`format_rfc3339`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn format_rfc3339(ms: i64) -> Result<String, errors::Error>` | Formats a `SystemTime` in RFC 3339 (`YYYY-MM-DDTHH:MM:SSZ`). |
| [`monotonic_ms`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn monotonic_ms() -> i64` | Monotonic clock reading in milliseconds. |
| [`monotonic_nanos`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn monotonic_nanos() -> i64` | Monotonic clock reading in nanoseconds. |
| [`now`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn now() -> i64` | Wall-clock milliseconds since the Unix epoch; `Instant::now()` reads the monotonic clock. |
| [`now_ms`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn now_ms() -> i64` | Wall-clock milliseconds since the Unix epoch. |
| [`now_nanos`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn now_nanos() -> i64` | Wall-clock nanoseconds since the Unix epoch. |
| [`parse_rfc3339`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn parse_rfc3339(text: String) -> Result<i64, errors::Error>` | Parses an RFC 3339 timestamp into a `SystemTime`. |
| [`since_ms`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn since_ms(instant: time::Instant) -> i64` | Milliseconds elapsed since an earlier monotonic reading. |
| [`sleep`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn sleep(ms: i64) -> ()` | Suspends the current goroutine for `Duration`. |
| [`unix_ms`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/time.rs) | `fn unix_ms() -> i64` | Current Unix time in milliseconds. |

```gos
use std::time

let ny = time::Location::lookup("America/New_York")?
let local = ny.civil(-1)?
match ny.resolve(local)? {
    time::CivilResolution::Unique(ms) => println!("{}", ms),
    time::CivilResolution::Gap => println!("nonexistent local time"),
    time::CivilResolution::Fold(earlier, later) => println!("{} {}", earlier, later),
}
```
