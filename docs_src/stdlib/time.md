# `std::time`

Status: shipped

Wall-clock and monotonic time facilities.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Instant` | type | Monotonic point-in-time. |
| `Duration` | type | Difference between two `Instant`s. |
| `SystemTime` | type | Wall-clock point-in-time. |
| `sleep` | fn | Suspends the current goroutine for `Duration`. |
| `now` | fn | Returns the current monotonic `Instant`. |
| `format_rfc3339` | fn | Formats a `SystemTime` in RFC 3339 (`YYYY-MM-DDTHH:MM:SSZ`). |
| `parse_rfc3339` | fn | Parses an RFC 3339 timestamp into a `SystemTime`. |
| `now_ms` | fn | Wall-clock milliseconds since the Unix epoch. |
| `now_nanos` | fn | Wall-clock nanoseconds since the Unix epoch. |
| `unix_ms` | fn | Current Unix time in milliseconds. |
| `monotonic_ms` | fn | Monotonic clock reading in milliseconds. |
| `monotonic_nanos` | fn | Monotonic clock reading in nanoseconds. |
| `since_ms` | fn | Milliseconds elapsed since an earlier monotonic reading. |

