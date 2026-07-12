# `std::lifecycle`

Status: experimental

Graceful-shutdown coordinator with signal handling and sd_notify support.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Lifecycle` | type | Registers shutdown hooks, listens for SIGTERM / SIGINT, and notifies systemd. |

