# PLAN-0001: Additive AFT-Go Sidecar For Deep-Beam Serving

## Status

- State: implemented
- Date: 2026-04-24
- Canonical answer surface: existing Rust AFT binary and plugin surface
- Go semantic target: warm `AFT-Go` sidecar behind a Rust provider seam
- Transport target: provider-agnostic in Rust; MCP sidecar is allowed, not mandated
- Implementation note: provider seam, provider-aware cache metadata, sidecar server mode, watcher-driven invalidation signaling, command freshness gates, and plugin/CLI adoption evidence are now landed in code

## Why This Plan Exists

AFT's goal is not cheap navigation. Its goal is to let the agent search the codebase with a beam that is both wide and deep.

The current startup problem is concentrated in the Go path:

- tree-sitter is already fast enough to be the base layer
- Rust command serving is already fast enough to stay the answer surface
- the expensive part is cold Go semantic analysis being launched repeatedly

The first version of this plan pushed toward "make all of AFT MCP-first". That is broader than necessary and increases fork drift against origin.

This revision narrows the change:

- keep Rust as the command and graph owner
- keep tree-sitter as the baseline
- extract the expensive Go work behind a provider seam
- let `AFT-Go` be the warm additive sidecar

That attacks the real bottleneck without rewriting the whole product around a new runtime shape.

## Product Constraints

These are binding for this plan:

1. `tree-sitter` stays.
2. Rust remains the canonical command and result surface.
3. `AFT-Go` is additive, not a replacement.
4. Rust owns the merged graph and final query serving.
5. `gopls` is not a required runtime dependency.
6. The design should minimize fork divergence from origin.
7. Deep Go-dependent answers must not silently degrade.
8. The plan must be resumable and parallelizable.
9. Implementation must proceed in TDD-like passes.

## Current Reality

The repo already has the right raw pieces:

- tree-sitter is the baseline resolver for all languages
- the Go helper contract is already additive and schema-based:
  - `docs/helper-contract.md`
- Rust already stores optional Go overlay data in `AppContext`:
  - `crates/aft/src/context.rs`
- `configure` already decides whether to run the helper sync or async:
  - `crates/aft/src/commands/configure.rs`
- `CallGraph` already merges helper output into Rust-owned indexes:
  - `crates/aft/src/callgraph.rs`
- the plugin already pools one warm Rust bridge per project:
  - `packages/opencode-plugin/src/pool.ts`

The current mismatch is narrower than "AFT lacks MCP":

- Go overlay production is still treated as a helper preflight
- helper caching is not feature-aware enough
- the Go producer is cold-started too often
- the Rust side does not yet have a transport-agnostic Go provider boundary

## Decision Summary

Build a Rust-side `GoOverlayProvider` seam and keep Rust as the owner of answers.

```text
Agent / Plugin / CLI
        |
        v
     AFT (Rust)
        |
        +-- tree-sitter base facts
        +-- CallGraph / merged indexes
        +-- cache / invalidation / freshness
        +-- command handlers / final responses
        |
        +-- GoOverlayProvider
              |
              +-- LocalHelperProvider   (current one-shot helper path)
              +-- AftGoSidecarProvider  (warm sidecar, MCP or other transport)
```

Important: Rust and Go are not interchangeable at the command layer.

The only thing that becomes swappable is the Go overlay producer.

Rust still owns:

- command routing
- result formatting
- merged graph construction
- cache metadata
- invalidation rules
- freshness policy

`AFT-Go` owns:

- Go package loading
- SSA / deep Go analysis
- warm project-scoped Go state
- overlay snapshot production

## What This Is Not

This plan does not do these things in the first wave:

- rewrite all of AFT into a new MCP-native runtime
- make Rust and Go both answer commands directly
- require `gopls`
- replace tree-sitter
- force CLI and plugin codepaths to be redesigned all at once
- replace `AppContext` and `CallGraph` with a brand-new architecture before the seam exists

## Ownership Boundary

### Rust owns

- tree-sitter parsing and file facts
- base graph and merged graph
- command handlers
- final tool responses
- on-disk cache metadata and validation
- watcher-driven invalidation
- freshness gates for deep commands

### AFT-Go owns

- Go module/workspace loading
- deep call resolution
- `dispatches`
- `dispatched_by`
- `implements`
- cross-package `writes`
- return-path and call-context augmentation
- warm Go-only caches internal to the sidecar

### Shared contract

The stable exchange boundary is still the helper schema family, centered on `HelperOutput` in:

- `crates/aft/src/go_helper.rs`

This is deliberate. Keeping the Rust merge path and wire schema recognizable reduces merge pain against origin.

## Provider Model

### Rust seam

Add a small Rust abstraction, for example:

```text
GoOverlayProvider
  id() -> provider id
  capabilities() -> feature support
  load_snapshot(request) -> snapshot or miss
  refresh(request) -> shared in-flight refresh handle
  poll(request) -> completion / status
```

The exact Rust API can vary, but the contract should support:

- cached snapshot load
- refresh request
- singleflight per project
- explicit feature flags
- explicit provider identity

### Provider backends

Backend 0:

- `LocalHelperProvider`
- wraps the current `resolve_for_root(...)`
- compatibility path
- fallback path

Backend 1:

- `AftGoSidecarProvider`
- talks to a long-lived Go process
- keeps per-project Go state warm
- returns the same logical overlay snapshot shape

### Sidecar lifetime and sharing

`AFT-Go` should be keyed by canonical project root, not by session.

That means:

- parallel agents working on the same repo should share one warm sidecar
- multiple plugin sessions on the same repo should share one warm sidecar
- the sidecar may outlive any one Rust client session
- idle retention is preferred over per-session startup/shutdown

This is the same broad pooling logic the plugin already uses for the Rust bridge; the difference is that this plan applies it to the Go semantic producer as well.

### Attach-time sanity check

When a Rust client attaches to an already-running `AFT-Go` sidecar, it must not blindly trust the warm state.

Before reuse, Rust and `AFT-Go` should compare at least:

- canonical project root
- provider id
- provider version / build id
- helper schema version
- feature flags
- Go env / build settings
- source fingerprint

If any of those differ, the sidecar state must be marked stale and refreshed before Rust treats it as fresh Go overlay data.

For the first implementation, the source fingerprint may be conservative:

- `.go` file mtimes and sizes
- `go.mod`
- `go.sum`
- `go.work`

The purpose is not perfect minimal invalidation yet. The purpose is to make cross-session reuse correct.

### Transport rule

Rust must not care whether the sidecar transport is:

- MCP
- NDJSON RPC
- stdio request/response
- unix socket / TCP loopback

The first implementation may use MCP if that is the fastest route, but the Rust seam must hide transport details so the rest of AFT stays stable.

### Provider wire protocol sketch

The provider seam should be backed by a small, explicit RPC surface. Keep the verbs narrow.

Minimum v1 verbs:

- `hello`
  - returns provider id, provider version, helper schema version, capabilities
- `status`
  - returns whether the sidecar has warm state for the requested root/features and whether that state is stale
- `refresh`
  - request refresh for a root/features/fingerprint tuple and return a job handle or synchronous completion
- `get_snapshot`
  - return the current overlay snapshot plus metadata used for Rust-side validation
- `invalidate`
  - inform the provider that Rust observed dirty files/packages/module state
- `shutdown`
  - best-effort explicit teardown for tests or process cleanup

Recommended request metadata:

- canonical project root
- feature flags
- provider request id
- source fingerprint
- optional dirty file list
- optional dirty package list
- `module_dirty`

Recommended response metadata:

- provider id
- provider version
- helper schema version
- env hash
- feature hash
- generated timestamp
- snapshot content hash
- stale / fresh status

### Sidecar discovery and lifecycle

The plan should be explicit about who starts `AFT-Go` and how Rust reuses it.

Rules:

1. Rust is responsible for discovery and attach.
2. One sidecar instance should map to one canonical project root.
3. A second Rust client for the same root should attach to the existing sidecar instead of starting a new one.
4. A sidecar should be kept alive with idle retention, not torn down at the end of each session.
5. Tests must be able to force shutdown deterministically.

Allowed implementation choices:

- sidecar spawned and tracked by the Rust bridge
- sidecar discovered through a local registry file keyed by canonical root
- sidecar discovered through a socket path / pid file / lock file

The exact mechanism can vary, but the lifecycle contract must hold:

- attach if reusable
- start if absent
- sanity-check on attach
- shutdown on explicit test cleanup or long idle timeout

Suggested lifecycle states:

```text
absent -> starting -> warming -> warm -> stale -> refreshing -> warm
                     \-> failed
```

The sidecar should be cheap to start and expensive only when warming.

## Data Model

### FileFacts

Rust-owned, tree-sitter-derived per-file facts:

- symbols
- imports
- local call sites
- language
- stat / parse metadata

### BaseGraph

Rust-owned graph derived from `FileFacts`:

- forward edges
- reverse edges
- symbol lookup
- file-to-symbol maps
- symbol-to-file maps

### GoOverlaySnapshot

Provider-produced overlay facts:

- helper schema version
- provider id
- env hash
- feature hash
- generated timestamp
- Go-only edges and annotations

The payload should remain compatible with `HelperOutput` as much as possible. Extra provenance belongs in side metadata, not in ad hoc command behavior.

### FreshnessState

Rust-owned freshness state should stay explicit:

```text
FreshnessState
  fs_epoch
  parse_epoch
  base_graph_epoch
  go_overlay_epoch
  dirty_files
  dirty_go_packages
  dirty_module_state
  active_refreshes
```

Rust remains the source of truth for whether an answer is fresh enough to serve.

## Cache Plan

Use the existing Rust cache as the durable serving cache. Let `AFT-Go` keep its own internal warm caches as an implementation detail.

Suggested Rust-side layout:

```text
$AFT_CACHE/<project-hash>/
  meta.json
  parse-index.cbor
  merged-graph.cbor
  go-overlay/
    <provider-id>/
      <env-hash>/
        <feature-hash>.json
        <feature-hash>.meta.json
```

### Rust cache responsibilities

Rust persists:

- parse cache
- merged graph
- imported Go overlay snapshots
- cache metadata used to validate those snapshots

### AFT-Go cache responsibilities

`AFT-Go` may keep:

- package loading state
- SSA artifacts
- internal memoization
- refresh journals

Rust must not depend on `AFT-Go`'s internal cache format.

This is an explicit boundary:

- Rust does not understand `AFT-Go`'s private cache layout
- Rust does not decide how `AFT-Go` refreshes its internal state
- `AFT-Go` does not decide whether Rust may serve stale deep Go results

Rust owns serving freshness. `AFT-Go` owns Go-analysis internals.

### Cache key requirements

The Rust-visible Go overlay cache key must include:

- `provider_id`
- helper/schema version
- provider binary or build version
- `go.mod`
- `go.sum`
- `go.work`
- relevant env and build settings
- feature flags:
  - call context on/off
  - return analysis on/off
  - dispatch edges on/off
  - implementation edges on/off
  - writes edges on/off

Without this, Rust can reuse an incompatible overlay snapshot.

## Invalidation Plan

### Source of truth

Use the existing filesystem watcher path in Rust.

Do not move invalidation authority into `AFT-Go`.

Rust is the source of truth for "something changed on disk". `AFT-Go` is the source of truth for "what internal Go work is required to refresh after that change".

### Event classes

Classify changes into:

- `parse_dirty(file)`
- `go_package_dirty(pkg)`
- `module_dirty`
- `noise`

Examples:

- `foo.ts` change -> `parse_dirty(file)`
- `pkg/service.go` change -> `parse_dirty(file)` + `go_package_dirty(pkg)`
- `go.mod` or `go.work` change -> `module_dirty`

### Effects

- `parse_dirty(file)`
  - invalidate Rust file facts
  - invalidate affected base graph slices
- `go_package_dirty(pkg)`
  - mark Go overlay stale for at least that package scope
  - first implementation may escalate to project-wide Go overlay dirty
- `module_dirty`
  - invalidate all Go overlay snapshots for the project

### Reattach invalidation

Warm sidecar reuse across sessions needs one more invalidation path:

- `reattach_mismatch`
  - triggered when Rust attaches to an existing `AFT-Go` sidecar and the attach-time sanity check detects mismatched root, version, env, features, or source fingerprint
  - effect: mark the sidecar state stale and require refresh before deep Go-dependent results are treated as fresh

### Invalidation contract to AFT-Go

Rust watcher events and attach-time checks must be translated into a provider-level invalidation signal.

For v1, the contract may be conservative. Rust can send any combination of:

- canonical project root
- feature flags
- `dirty_files`
- `dirty_packages`
- `module_dirty`
- `source_fingerprint`

`AFT-Go` then decides the refresh details:

- whether to reuse loaded packages
- whether to rebuild package-local state
- whether to fall back to project-wide refresh
- how to update its private cache

Rust should not reach inside and orchestrate those details. Rust only needs enough metadata back to know whether the returned overlay snapshot is valid to serve.

### Serving rules

- `outline`, `read`, `grep`, `glob`
  - never wait on Go overlay freshness
- `zoom`
  - requires fresh file parse data
  - waits for Go overlay when Go-specific augmentation is requested
- Go-dependent behavior tools
  - require fresh Rust base graph
  - require fresh Go overlay when the query semantics depend on Go

## Decided Defaults

These were previously open questions. For this plan, they are now fixed defaults:

- `zoom` waits for relevant Go augmentation by default
- Rust persists snapshots, not dirty scheduler state, across restart
- there is no public `allow_stale` flag in the first wave
- stale deep Go-dependent results must be surfaced explicitly if ever served internally

## Scheduler And Refresh Model

Keep this narrow in the first wave.

Rust should coordinate:

- one in-flight Go overlay refresh per project and feature set
- coalesced parse invalidation
- explicit waiting only on relevant Go-dependent commands

Required rules:

1. Unrelated dirty work must not block unrelated commands.
2. Generic startup must not synchronously force a full Go refresh.
3. Repeated deep Go queries should benefit from the warm sidecar.
4. Concurrent requests for the same Go refresh should singleflight.
5. Reattaching to an existing sidecar must perform the attach-time sanity check before reuse.

## Failure Policy

Failure behavior should be explicit, because this seam is additive but important.

### Hard rules

- Rust remains the final authority on whether a deep Go-dependent answer may be served.
- A missing or failed sidecar must not crash generic tree-sitter-backed commands.
- Deep Go-dependent commands must not silently downgrade when fresh Go semantics are required.

### Fallback rules

- if `AFT-Go` is unavailable and `LocalHelperProvider` is available:
  - Rust may fall back to the local helper path
- if both sidecar and local helper are unavailable:
  - Rust should return a clear failure for Go-dependent deep queries
- if the sidecar is alive but stale:
  - Rust should request refresh and wait when the command requires fresh Go semantics
- if the sidecar times out repeatedly:
  - Rust may mark the provider unhealthy and use fallback if available

### Timeout and restart policy

- provider calls need explicit timeout budgets
- restart attempts must be bounded
- crash loops must be surfaced in logs and metrics
- fallback should be deterministic, not best-effort magic

### Caller-visible behavior

The result surface should make these cases distinguishable:

- fresh sidecar-backed answer
- fresh fallback-local-helper answer
- refused because fresh Go semantics could not be produced

## Observability

This seam is operationally subtle. Without telemetry, debugging will be guesswork.

At minimum, log and expose counters for:

- provider selected
- sidecar attach success
- sidecar attach mismatch reason
- sidecar start count
- sidecar warm reuse count
- refresh start / finish / failure
- refresh duration
- snapshot cache hit / miss
- fallback-to-local-helper count
- deep-query refusal count

Useful structured fields:

- canonical root
- provider id
- provider version
- feature hash
- env hash
- source fingerprint hash
- refresh scope
- timeout / error reason

The goal is not analytics. The goal is to make correctness and latency regressions diagnosable.

## `gopls` Position

`gopls` is still not a required runtime dependency.

Allowed roles:

- optional oracle in tests
- optional comparison tool during development
- optional editor-only sidecar if desired later

Disallowed role:

- mandatory dependency for core AFT behavior commands

## TDD-Like Passes

Every pass must follow the same rule:

1. add or tighten tests first
2. make them fail for the intended reason
3. implement the smallest slice that makes them pass
4. refactor only after green
5. update the status board with evidence

### Pass P00: Characterization And Guard Rails

Goal:

- lock behavior that must survive the seam extraction
- lock new provider contracts before implementation

Tests first:

- Rust remains the final answer surface
- tree-sitter-only commands do not require Go overlay
- helper cache key includes provider and feature dimensions
- provider capability checks exist
- no command handler directly depends on transport details
- provider verbs and response metadata are nailed down by tests or fixtures
- failure outcomes are explicit for deep Go-dependent commands

### Pass P01: Rust Provider Seam

Goal:

- introduce `GoOverlayProvider` with no intended command behavior change

Tests first:

- default provider is the local helper backend
- provider selection is explicit and testable
- `configure` and `AppContext` can depend on the trait, not a concrete helper path
- provider request/response types are transport-agnostic

Implementation target:

- new Rust provider module
- adapt `context.rs`
- adapt `configure.rs`

### Pass P02: Provider-Aware Cache Schema

Goal:

- make Rust-side Go overlay caching provider-aware and feature-aware

Tests first:

- cache key varies by provider id
- cache key varies by module/env inputs
- cache key varies by feature flags
- incompatible snapshots are rejected cleanly

### Pass P03: AFT-Go Sidecar Skeleton

Goal:

- add a warm Go sidecar path without removing the local helper fallback

Tests first:

- one project can reuse one warm sidecar
- concurrent requests singleflight correctly
- repeated requests avoid full cold re-analysis
- sidecar reuse across sessions requires attach-time sanity validation
- sidecar discovery/attach/start rules are enforced
- idle retention and explicit shutdown are both testable
- fallback to local helper remains possible

Implementation target:

- evolve `go-helper/` into a sidecar-capable producer, or add a sibling Go binary
- keep the existing snapshot schema recognizable

### Pass P04: Freshness And Invalidation Integration

Goal:

- make Rust invalidation drive provider refresh decisions

Tests first:

- Go file changes mark Go overlay stale
- module changes invalidate all overlay snapshots
- unrelated non-Go changes do not block unrelated commands
- Go-dependent commands wait on relevant refreshes

### Pass P05: Command Routing On Top Of The Seam

Goal:

- serve Go-augmented commands through the provider-backed overlay path

Tests first:

- `implementations`
- `dispatches`
- `dispatched_by`
- `writers`
- `call_tree`
- `callers`
- `trace_to`
- `trace_data`
- `impact`
- Go-augmented `zoom`

Each should prove:

- Rust still serves the final answer
- no silent downgrade on stale Go semantics
- expected wait behavior after relevant edits

### Pass P06: Surface Adoption And Incremental Deepening

Goal:

- adopt the seam in existing plugin / CLI surfaces
- narrow Go refresh scope only after correctness is proven

Tests first:

- plugin surface can use the sidecar provider without changing output shape
- CLI can still function when only the local helper backend is present
- package-scoped refresh is allowed only when proven correct
- observability hooks exist for provider selection, refresh, fallback, and refusal

## Parallel Work Packets

Do not run packets in parallel when they share the same write scope.

```text
+------+--------------------------------+--------------------------------------------------+-------------+
| ID   | Packet                         | Primary write scope                              | Depends on  |
+------+--------------------------------+--------------------------------------------------+-------------+
| W00  | Characterization tests         | crates/aft/tests/, go-helper tests               | none        |
| W01  | Rust provider seam             | crates/aft/src/go_overlay_provider*.rs           | W00         |
| W02  | Rust cache schema              | crates/aft/src/persistent_cache.rs, go_helper.rs | W00         |
| W03  | AFT-Go sidecar                 | go-helper/ or sibling Go binary                  | W00         |
| W04  | Freshness / invalidation       | crates/aft/src/context.rs, main.rs, callgraph.rs | W01,W02     |
| W05  | Command routing                | crates/aft/src/commands/                         | W01,W02,W04 |
| W06  | Plugin / CLI adoption          | packages/opencode-plugin/src/, wrapper scripts   | W01,W03     |
| W07  | Docs / migration / status      | docs/, this file                                 | all passes  |
+------+--------------------------------+--------------------------------------------------+-------------+
```

### Packet notes

- `W00` lands first and defines the contracts.
- `W01` should stay additive; avoid broad refactors in this pass.
- `W02` and `W03` can run in parallel after `W00`.
- `W04` should not redesign command handlers; it should expose reusable freshness logic.
- `W06` should focus on adoption, not architecture.

## Resume Protocol

Any future worker should do this before editing code:

1. Read this file top to bottom.
2. Read:
   - `docs/helper-contract.md`
   - `crates/aft/src/go_helper.rs`
   - `crates/aft/src/context.rs`
   - `crates/aft/src/callgraph.rs`
   - `crates/aft/src/persistent_cache.rs`
   - `crates/aft/src/commands/configure.rs`
   - `packages/opencode-plugin/src/pool.ts`
3. Identify the first incomplete pass whose dependencies are satisfied.
4. Claim exactly one work packet by updating the status board.
5. Add failing tests first.
6. Stay inside the packet's write scope unless the packet explicitly requires more.
7. Before handoff:
   - mark pass or packet status
   - list tests added
   - list tests run
   - record blockers in this file

## Status Board

```text
+------+--------------------------------------------+-------------+-------------------------------+
| ID   | Item                                       | Status      | Evidence                      |
+------+--------------------------------------------+-------------+-------------------------------+
| P00  | Characterization and guard rails           | done        | new unit tests in go_overlay.rs; existing helper cache tests still pass |
| P01  | Rust provider seam                         | done        | crates/aft/src/go_overlay.rs, config.rs, context.rs, configure.rs |
| P02  | Provider-aware cache schema                | done        | GoOverlaySnapshotMeta + wrapped go-helper cache + helper-input-hash write |
| P03  | AFT-Go sidecar skeleton                    | done        | go-helper/main.go sidecar mode + go-helper/rpc.go + go-helper/rpc_test.go |
| P04  | Freshness and invalidation integration     | done        | main.rs watcher -> go_overlay invalidate_provider; attach-time hello/status checks |
| P05  | Command routing on top of the seam         | done        | deep Go commands now gate on `require_go_overlay*`; integration test covers explicit `go_overlay_unavailable` refusal |
| P06  | Surface adoption and incremental deepening | done        | plugin config/status surfaces already accept and render `go_overlay_provider`; focused Bun tests pass; Rust status now drains background overlay completion and reports terminal failure state |
+------+--------------------------------------------+-------------+-------------------------------+
```

## Acceptance Criteria

This plan is complete only when all of the following are true:

1. Rust still serves every AFT command as the final answer surface.
2. tree-sitter remains the baseline graph input.
3. Go overlay facts enter Rust through a provider seam, not direct ad hoc helper wiring.
4. `AFT-Go` can stay warm across multiple requests and multiple sessions for the same repo.
5. Rust-visible Go overlay caches are provider-aware, env-aware, and feature-aware.
6. Go-dependent commands wait on relevant freshness instead of silently degrading.
7. A fallback path exists when the warm sidecar is unavailable.
8. Reused warm sidecars are validated by an attach-time sanity check before being treated as fresh.
9. The plugin / CLI surfaces can adopt the seam without forcing a whole-project runtime rewrite.
10. Failure and fallback behavior for deep Go-dependent commands is explicit and tested.
11. Observability exists for attach, refresh, fallback, and refusal paths.
12. Tests lead each pass.

The first-wave implementation is intentionally conservative about refresh scope. Package-scoped Go refresh remains a future optimization, not a blocker for completion of this plan.

## Remaining Engineering Questions

These do not block the first passes:

- how narrow can Go invalidation become before correctness suffers?
- which transport is simplest to maintain long-term once the Rust provider seam exists?

## References

- `docs/helper-contract.md`
- `crates/aft/src/go_helper.rs`
- `crates/aft/src/context.rs`
- `crates/aft/src/callgraph.rs`
- `crates/aft/src/persistent_cache.rs`
- `crates/aft/src/commands/configure.rs`
- `packages/opencode-plugin/src/pool.ts`
- `packages/opencode-plugin/src/bridge.ts`
- `docs/ADR-0004-persistent-graph.md`
- `/Users/nanda/Documents/projects/personal/codebase-memory-mcp/README.md`
- `/Users/nanda/Documents/projects/personal/codebase-memory-mcp/src/pipeline/pipeline_incremental.c`
- `/Users/nanda/Documents/projects/personal/codebase-memory-mcp/src/watcher/watcher.c`
