# Incremental frontend compile — staged rollout

Goal: on a re-run with unchanged source, `gos run` / `gos check` /
`gos test` / `gos build` should skip parse + resolve + typecheck
+ HIR lowering and reuse the previous result. Today the first two
phases ship: every `gos check --timings` prints per-stage
wall-clock, and every cache hit short-circuits parse.

## Phase 1 — infrastructure (shipped)

- `gossamer_driver::FrontendCacheKey` — content-addressed key
  combining source bytes with the driver's `CARGO_PKG_VERSION`.
- `cache_dir()` — resolves the on-disk cache root. Honours
  `GOSSAMER_CACHE_DIR`, then `XDG_CACHE_HOME`, then
  `$HOME/.cache`, then a workspace-relative fallback.
- `mark_success(&key)` / `observe_hit(&key)` — marker files for
  cache-hit observability.
- `_in(dir, key)` variants for tests and workspace-local caches.
- `GOSSAMER_CACHE_TRACE=1` prints hit/miss lines.

## Phase 2 — measurement (shipped)

`gos check --timings` prints parse / resolve / typeck / exhaust /
total wall-clock and the input byte count. Fed into the bench
harness so we can justify the Phase 3 scope.

## Phase 3 — parse-skip (shipped)

`gossamer_driver::store_blob` / `load_blob` persist a `SourceFile`
as a bincode blob under the cache directory. On hit,
`load_or_parse` skips `gossamer_parse::parse_source_file` and
returns the cached AST. Serde derives live on every AST type plus
`Span` / `NodeId` / `FileId`.

Cache layout:

```
$cache/gossamer/frontend/
  <sha256>.ok        zero-byte marker — "we've compiled this key"
  <sha256>.bin       bincode-encoded SourceFile
```

## Phase 4 — skip resolve + typecheck + HIR (pending)

Deliver a real full-frontend skip by serializing the rest of the
pipeline: `Resolutions`, `TypeTable`, `TyCtxt` (with the interned
type arena), and `HirProgram`. The challenges:

- `TyCtxt` holds interned `Ty` handles (indices into an arena).
  Round-tripping requires either rematerialising the arena or
  serialising it verbatim and accepting that the indices stay
  valid across runs.
- Every downstream type would need `Serialize` / `Deserialize`,
  mirroring the AST roll-out in Phase 3.

Design option (bincode "everything"): dump the full
`(Resolutions, TypeTable, TyCtxt, HirProgram)` tuple as a single
blob. Fast and mechanical, at the cost of a schema bump on every
new pipeline field.

Design option (per-stage, revalidated): dump each stage
separately with an explicit version. Slower, more robust to
refactors.

Recommendation: start with bincode-everything; switch to
per-stage if the schema churn becomes painful.

## Failure modes

- Corrupt cache entry — silently re-run the pipeline, overwrite
  the entry. The `.ok` marker distinguishes "never seen" from
  "tried and failed."
- Disk full — fall back to no-cache. The cache is advisory.
- Concurrent writers — two `gos` invocations on the same file.
  Write-then-rename so a partial write never shadows a good blob.

## Non-goals

- Workspace-level incremental (`gossamer_driver::BuildCache`
  already covers that at the crate × target × profile level).
- Cross-file invalidation. A change to module A should
  invalidate dependants of A — requires a dep graph at the
  frontend level, a post-Phase-4 item.
- Compressed cache. bincode + zstd would shrink the blobs but
  complicates the open-a-blob-and-peek debugging story.
