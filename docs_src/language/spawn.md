# `lang::spawn`

Goroutine spawn: `spawn(f)` -> `JoinHandle<T>`, `.join()` -> `Result<T, String>`. The child attaches to the `cohort { }` the call is written inside, which every `spawn` outside `main` must have (GT0086), so nothing is detached and nothing is attached by accident.
