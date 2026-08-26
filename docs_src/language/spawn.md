# `lang::spawn`

Goroutine spawn: `spawn(f)` -> `JoinHandle<T>`, `.join()` -> `Result<T, String>`. The child attaches to the enclosing cohort, so nothing is detached.
