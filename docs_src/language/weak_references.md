# `lang::weak_references`

Status: experimental

`Weak<T>` downgrade/upgrade handles. Native collection is thread-local only and the bytecode VM has no cycle collector, so cross-tier cyclic reclamation is not yet a Stable guarantee.
