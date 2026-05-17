# `std::context`

Status: shipped

Request-scoped cancellation, deadlines, and timeouts.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Context` | type | Cancellation-aware context handle. |
| `background` | fn | Root context — never cancelled. |
| `with_cancel` | fn | Child context plus explicit cancel handle. |
| `with_deadline` | fn | Child context that cancels at the supplied instant. |
| `with_timeout` | fn | Child context that cancels after the supplied duration. |

