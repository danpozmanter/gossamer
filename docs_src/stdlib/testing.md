# `std::testing`

Status: shipped

Assertions and sub-test harness helpers.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Runner` | type | Sub-test collector. |
| `check` | fn | Asserts a condition. |
| `check_eq` | fn | Asserts equality, rendering a diff on failure. |
| `check_ok` | fn | Asserts a Result is Ok, recording without panicking. |
| `wait_for_scheduler_idle` | fn | Waits for the scheduler to become idle within a timeout. |

