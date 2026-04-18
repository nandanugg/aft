# DESIGN — Persistent merged graph + incremental updates (Tier 2)

Status: design (not implemented)
Scope: disk-backed cache for the reverse call index + helper output. Incremental updates keyed on per-file `(mtime, size)`.
Builds on: the existing per-file parse cache plan (see `project_aft_disk_cache_plan` memory).

## Motivation

Each `aft` CLI invocation today spawns a fresh Rust process that:
1. Walks every project file.
2. Re-parses every file with tree-sitter (hundreds to ~1000 files for a typical Go service = multi-second wall clock).
3. Runs the Go helper against the whole project (adds another 5–60 seconds depending on size).
4. Builds the reverse index in memory.
5. Answers the query.
6. Throws it all away.

Repeat invocations within the same editing session (very common for agents: ten `aft` calls in a minute) pay the full cold-start cost every time. Users perceive AFT as "slow" on large projects despite the actual query being < 10ms.

The fix is a disk cache keyed on content staleness, with separate invalidation rules for the two data sources (tree-sitter parse = per-file, helper output = per-project).

## Design principles (binding)

1. **Correctness over cleverness.** If we can't prove a cache hit is valid, we rebuild. A spurious invalidation is fine; a stale hit that reports wrong call edges is a correctness regression.
2. **Atomic writes.** Cache files update via `write(tmp); rename(tmp, final)`. No partial writes visible.
3. **Corruption recovery.** Corrupted or version-mismatched cache files cause full rebuild, never a crash.
4. **Separate staleness for parse cache vs helper cache.** Tree-sitter cache invalidates per-file; helper cache invalidates per-project (for now — see Open questions for per-package incremental helper).
5. **Cache is an optimization, never a correctness dependency.** `--no-cache` flag forces fresh parse + fresh helper run. CI should default to `--no-cache`.
6. **No background processes.** No daemons, no file watchers. Each invocation reads cache, checks staleness, uses or rebuilds. Keeps the "just a CLI tool" mental model.

## Storage layout

```
$CACHE_ROOT/                                      # default: ~/.cache/aft/
└── <project-hash>/                               # sha256(abs_project_root)[..12]
    ├── meta.json                                 # project root, aft version, schema version
    ├── parse-index.cbor                          # per-file tree-sitter parse cache
    ├── helper-output.json                        # last full helper output
    ├── helper-input-hash                         # hex digest of file list + mtimes at helper run
    └── merged-graph.cbor                         # derived reverse index (rebuilt from parse + helper)
```

`$CACHE_ROOT`:
- Default: `~/.cache/aft/` (XDG-compliant on Linux, same on macOS).
- Override: `AFT_CACHE_DIR` env var.
- Plugin context: when running under a plugin that sets `storage_dir`, use that directly.

`<project-hash>`:
- `sha256(canonical_abs_root)`, hex, first 12 chars.
- Collision resistance at 12 chars is ~10^14 projects before 50% collision — fine.

### `meta.json`

```json
{
  "project_root": "/Users/nanda/...",
  "aft_version": "0.4.2",
  "schema_version": 1,
  "created_at": "2026-04-18T12:34:56Z",
  "last_refreshed_at": "2026-04-19T08:22:10Z"
}
```

Purpose: sanity-check. If `aft_version` or `schema_version` differs from the running binary, discard the cache. Prevents format-drift issues after upgrade.

### `parse-index.cbor`

CBOR-encoded `HashMap<RelPath, (FileStat, FileParse)>` where:
- `FileStat = { mtime_nsec: i128, size: u64 }`
- `FileParse` = the existing tree-sitter parse result (whatever shape it has today).

CBOR not JSON: per-file data is non-trivial (AST fragments, symbol tables). CBOR is binary, ~3–5x smaller and ~10x faster to decode than JSON. `serde_cbor` or `ciborium` in Cargo.toml.

On load: memory-map the file, stream-decode. For a 1000-file project, expect 10–50MB decoded, 100–500ms cold load. Small projects < 50ms.

### `helper-output.json`

Plain JSON matching the exact format `aft-go-helper` emits. Cache hit rule: the file list + per-file `(mtime, size)` hash (`helper-input-hash`) must match the current project state.

Why not CBOR: the helper writes JSON natively; we cache its output verbatim. No transcoding.

### `helper-input-hash`

Single hex line. Contents: `sha256(concat(sorted(rel_path + mtime_nsec + size for each .go file)))`.

On each invocation:
1. Enumerate project `.go` files.
2. `stat()` each (~microseconds × ~1000 files = ~5–10ms total).
3. Compute this hash.
4. Compare to cached hash.
5. Match → use `helper-output.json` as-is. Mismatch → re-run helper.

### `merged-graph.cbor`

The derived data structure (reverse index, forward index, dispatch-key secondary index, implementation index) built from tree-sitter parse + helper output.

**Important:** merged-graph is *derived*, never authoritative. If it's missing or stale, rebuild from parse-index + helper-output (fast: < 500ms for a large project, because neither parsing nor helper runs). If parse-index or helper-output is stale, refresh those first, then rebuild merged-graph.

Cache hit rule: both parse-index and helper-output hashes present and valid, and `merged-graph.cbor`'s embedded header matches.

## Invalidation flow

```
start of aft invocation
  │
  ├─ read meta.json — schema/version mismatch? → delete cache, full rebuild
  │
  ├─ walk project files, stat() each
  │
  ├─ for each file: compare (mtime, size) vs parse-index
  │     match  → reuse cached parse
  │     miss   → re-parse with tree-sitter, update parse-index entry
  │     missing-on-disk but cached → drop entry
  │
  ├─ compute helper-input-hash over current file states
  │     match cached helper-input-hash → use cached helper-output.json
  │     mismatch → re-run aft-go-helper, update helper-output.json + hash
  │
  ├─ merged-graph.cbor present AND both parse + helper caches unchanged since last merge?
  │     yes → memory-map merged-graph, done
  │     no  → rebuild merged-graph from parse-index + helper-output, atomic write
  │
  └─ answer the query
```

## Atomic write invariant

Every cache file update:

```rust
let tmp = cache_dir.join(format!("{}.tmp.{}", final_name, std::process::id()));
write_all(&tmp, data)?;
fsync(&tmp)?;
rename(&tmp, &final_path)?;
```

- `fsync` before rename to ensure data is durable before the swap.
- `rename` is atomic on same-filesystem (POSIX, ext4, apfs, zfs, tmpfs). Different filesystems would need `copy + rm`, not supported here (cache should live on the same FS as `$HOME`).
- PID in tmp name avoids concurrent-aft-process collisions.

## Corruption recovery

Every read:
- Parse errors → log warning, delete file, rebuild.
- Size-zero file → treat as corrupted (happens after crash between `write` and `fsync`).
- Version mismatch → log, delete, rebuild.

Never crash on cache read. Cache is always optional.

## Concurrency

Two `aft` processes running simultaneously against the same project:
- Reading is safe (CBOR files are read-only).
- Writing race: if both decide to re-parse and write, the later `rename` wins. Neither crashes; data integrity preserved.
- No file locking — AFT is a query tool, not a database. "Last writer wins" is the right semantic. File-level atomicity is sufficient.

Explicit non-goal: **do not** build cross-process coordination (locks, PID files). Real multi-process use (CI, parallel agent runs) is rare enough that "both processes do the same work twice" is an acceptable cost.

## Performance budget

| Metric | Target | Rationale |
|---|---|---|
| Cold-start penalty (first run) | < 10% over no-cache | We do extra bookkeeping (compute hashes, write cache). |
| Warm-start time (no file changes) | < 300ms | Load meta + stat files + mmap parse-index + mmap merged-graph. |
| Single-file edit re-parse | < 100ms | Tree-sitter on one file + merged-graph patch. |
| Single-file edit helper penalty | < 30s on 100KLoC project | Helper re-runs whole project. |
| Cache disk footprint | < 100MB per project | 1000-file project averages 50–80MB with parse + helper + merged. |

If the 300ms warm-start target is missed, the cache is not delivering its value — revisit.

## Incremental helper (deferred, see Open questions)

The hard question is whether the helper can run per-package instead of whole-project. SSA + CHA are global analyses (interface resolution crosses package boundaries). A true incremental helper would need to invalidate only *downstream* packages that import the changed package — tractable via Go's build graph, but non-trivial.

**First implementation: full project re-run on any change.** This doc fixes the common case (repeat queries with no changes = fast). Incremental helper is a separate future design doc.

## Rollout / feature flag

- Rust: `[cache] enabled = true` config knob. Default `true`.
- CLI: `--no-cache` forces cache-less run (useful for debugging, CI).
- Env: `AFT_DISABLE_CACHE=1` as a kill switch.
- Cache-version-bump procedure: increment `schema_version` in Rust source; all existing caches auto-invalidate.

## Tests

1. **Round-trip**
   - Build cache, re-open, verify all queries return identical results.

2. **Invalidation**
   - Edit a file (touch mtime) → that file re-parses, others reused.
   - Delete a file → entry removed from parse-index.
   - Rename a file → old entry dropped, new entry parsed.

3. **Corruption**
   - Truncate parse-index mid-file → detected, rebuild, no crash.
   - Zero-byte merged-graph → rebuild.
   - meta.json with wrong schema_version → full cache discard.

4. **Concurrent access**
   - Two `aft` processes invoked simultaneously → both succeed, no data loss, final state consistent.

5. **Benchmark**
   - `aft callers` on 1000-file Go project: cold vs warm timings.
   - Target: warm run 10x+ faster than cold.

## Open questions for the implementer

1. **Cache format for `FileParse`:** the current in-memory representation may not be serde-friendly. If refactoring is required, keep the serialized form stable across aft versions that share `schema_version`. Bump `schema_version` on any change.

2. **Incremental helper:** deferred explicitly. If an implementer finds a cheap path to per-package helper invocation (e.g., helper accepts `-package=pkg1,pkg2` and only reruns those + downstream), that's a separate PR + design doc, not this one.

3. **Shared cache across projects:** if two projects share source files (rare but possible with git worktrees), each project has its own cache dir. No cross-project sharing. Simpler + safer.

4. **`search_index` / `semantic_index` locations:** existing memory plan mentions these. Align the new caches with the same `$CACHE_ROOT/<project-hash>/` structure so there's one cache dir per project across all AFT features.

## Summary

Adds `$CACHE_ROOT/<project-hash>/` with parse cache (CBOR, per-file keyed on mtime+size), helper output cache (JSON, keyed on file-list hash), and derived merged graph (CBOR). Atomic writes, corruption-safe, no daemons, no cross-process coordination. Target: warm runs 10x faster than cold. Full-project helper re-run on file changes is accepted for the first implementation; per-package incremental is a future design doc.
