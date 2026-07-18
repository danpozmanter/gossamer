# Eager versus lazy iterators

Build this edition-2027 project in release mode and time the executable:

```sh
gos build --release benchmarks/lazy_iterators
time benchmarks/lazy_iterators/target/release/lazy_iterators
```

`expected.txt` pins semantic equality. To inspect materialization, run with
`GOS_VEC_ALLOC_STATS=1`; the lazy side allocates no intermediate Vec.
