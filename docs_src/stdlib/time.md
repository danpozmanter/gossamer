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

