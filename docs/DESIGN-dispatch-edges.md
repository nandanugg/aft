# DESIGN — Dispatch edges (Tier 1.1 + 1.2 + 1.3)

Status: design (not implemented)
Scope: Go helper schema additions, Rust-side deserialization, two new `aft` commands, one filter-at-source rule addition.
Successor to: [`helper-contract.md`](helper-contract.md) — this doc extends it; the parent contract still governs everything unchanged here.

## Motivation

Go services use **registration-time dispatch** patterns everywhere: `asynq.HandleFunc("TypeX", handler)`, `mux.HandleFunc("/path", h)`, `consumer.Register(topic, h)`, `gocron.Every(...).Do(h)`. Tree-sitter sees a call (`asynq.HandleFunc(...)`) but cannot tell that `handler` runs later, keyed by `"TypeX"`. Go's SSA can resolve `handler` as a function value passed as an argument — the helper should expose that.

Measurement from the aft-hooked-vs-cbm comparison showed AFT losing 16 claims on asynq task-queue accuracy that CBM got right, entirely because CBM's graph has these edges and AFT's does not. Adding them closes the gap.

## Design principles (binding)

1. **Library-agnostic.** No hardcoded list of "dispatch libraries". If SSA sees a function value passed as an argument, that's a dispatch edge. Works for asynq, kafka, grpc, net/http, gocron, tomorrow's library X, and in-house frameworks alike.
2. **Filter at source** ([helper-contract.md §Filter-at-source rule](helper-contract.md)). Every dispatch edge crossing the stdout pipe is memory the Rust side must hold. Emit only what tree-sitter genuinely cannot reconstruct.
3. **Additive, no breaking changes.** New `EdgeKind` variants + new optional field on `HelperEdge`. Old Rust + new helper still works (unknown kind → retained per existing rule). Old helper + new Rust still works (missing optional field → `None` via `serde(default)`).
4. **Schema version stays at `v1`.** Bump is reserved for breaking changes. This is purely additive.
5. **Helper-side filtering > Rust-side post-processing.** If a dispatch edge can't be useful (anonymous closure with no symbol, handler outside project), drop in the helper before serialization.

## Data model additions

### 1.1 — Dispatch edges (function-value-as-argument)

In SSA, when the helper walks a `ssa.CallCommon`, for each argument that is a function value (`*ssa.Function`, `*ssa.MakeClosure` bound to a named function, method expression `t.Method`), emit a new edge with `kind: "dispatches"`.

**SSA types that qualify as function values:**
- `*ssa.Function` — direct function reference (`handler` where `handler` is a package-level func).
- `*ssa.MakeClosure` where the bound function has a non-anonymous name — captures `func() { ... }` assignments to a named var, and method-value expressions `t.Method`.

**SSA types that do NOT qualify:**
- Anonymous closures with no corresponding named function (literal `func(){}` inline). Emitting these would require synthesizing a "caller+lineN" identifier — too fragile, no useful query target.
- Function values whose concrete target SSA could not resolve (dynamic lookup from map, reflection). Skip rather than emit a guess.

### 1.2 — String-proximity key (`nearby_string`)

When emitting a `dispatches` edge, scan the *same call's* argument list for string literal arguments (`*ssa.Const` with a string value). If exactly one exists and its value is ≤ 128 chars, attach it as `nearby_string` on the edge.

**Why "exactly one":**
- Zero strings → no key to attach, leave field absent.
- One string → the overwhelming real-world case (`HandleFunc(keyString, handlerValue)`).
- Two or more strings → ambiguous (e.g. `rpc.Register(name, category, handler)` — we don't know which is the dispatch key). Drop rather than guess. Agents can still find the edge via `dispatched_by`.

**Why ≤ 128 chars:**
- Dispatch keys in the wild are short tokens ("TypeX", "/path", "user.login").
- Long strings are almost always error-message format strings — exactly CBM's 94-of-102 false-positive failure mode on their Route nodes.
- 128 is generous; the rule is "cap to avoid format-string pollution", not "this is the real max".

### 1.3 — Edge variant tagging

Today `EdgeKind` is `interface | concrete | static` (`static` is filtered-out by contract). Extend to distinguish *how* a call happens:

| Kind | Semantics | SSA origin |
|---|---|---|
| `interface` | unchanged — dynamic interface dispatch | CHA targets of `*ssa.Call` with interface-method callee |
| `concrete` | unchanged — resolved concrete-method call | `*ssa.Call` with `*ssa.Function` callee |
| `static` | filtered by contract (tree-sitter already resolves) | n/a |
| **`dispatches`** (new) | function value passed as an argument | rule above |
| **`goroutine`** (new) | `go fn()` | `*ssa.Go` instruction |
| **`defer`** (new) | `defer fn()` | `*ssa.Defer` instruction |

**Filter rule for `goroutine` and `defer`:** emit only when the spawned/deferred function is in-project and the caller→callee edge would *otherwise be dropped* as same-file/same-package (currently the helper filters these as "tree-sitter already resolves"). For goroutines/defers, the variant itself is the added information — tree-sitter sees `go f()` syntactically but doesn't surface that distinction.

Concretely:
- If tree-sitter already emits a `CALLS` edge for `f()` (same-file), we still emit a `goroutine`/`defer` edge *because the variant carries information*. The Rust side dedups on `(caller, callee, kind)` — `kind` differing keeps both records.
- If the target of the goroutine is outside the project, drop (same filter rule as concrete).

## JSON schema additions

Extend `HelperEdge` with one optional field. `EdgeKind` gains three variants.

**Before (v1):**
```json
{
  "caller": {"file": "...", "line": 42, "symbol": "..."},
  "callee": {"file": "...", "symbol": "...", "receiver": "...", "pkg": "..."},
  "kind": "interface"
}
```

**After (still v1, additive):**
```json
{
  "caller": {"file": "...", "line": 42, "symbol": "..."},
  "callee": {"file": "...", "symbol": "...", "receiver": "...", "pkg": "..."},
  "kind": "dispatches",
  "nearby_string": "TypeMerchantSettlementV3"
}
```

Field:
- `nearby_string` — optional string. Present only on `dispatches` edges where exactly one string literal ≤ 128 chars appears in the same call's arg list. Absent otherwise. Not emitted for `interface`, `concrete`, `goroutine`, `defer`.

`kind` extension:
- Adds `"dispatches"`, `"goroutine"`, `"defer"` as valid values.
- Rust must continue to handle unknown kinds by retaining the edge (existing contract).

## Rust-side changes

### `crates/aft/src/go_helper.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperEdge {
    pub caller: HelperCaller,
    pub callee: HelperCallee,
    pub kind: EdgeKind,
    /// Optional dispatch-key string for `kind == Dispatches`. Absent on
    /// other kinds. Capped at 128 chars at the helper.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nearby_string: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum EdgeKind {
    Interface,
    Concrete,
    /// Filtered by helper contract; reserved for future diagnostic use.
    Static,
    /// NEW: function value passed as an argument (registration / callback).
    Dispatches,
    /// NEW: `go fn()` — spawned asynchronously.
    Goroutine,
    /// NEW: `defer fn()` — runs on enclosing function return.
    Defer,
}
```

`EdgeKind` does **not** need a `#[serde(other)]` variant — existing behavior of rejecting unknown kinds is preserved via the "Rust treats unknown kinds conservatively" contract (i.e. serde deserializes the string into a stored-as-string fallback field). If a `#[serde(other)]` pattern is needed for forward-compat, it belongs in a separate design doc.

### `crates/aft/src/callgraph.rs`

The reverse index today keys edges by `(callee_symbol, callee_file)` → list of callers. Extend the caller-side record with edge kind:

```rust
// existing approximate shape (the real struct has more fields):
pub struct ReverseCaller {
    pub file: PathBuf,
    pub symbol: String,
    pub line: u32,
    pub kind: EdgeKind,          // already there per the existing interface/concrete distinction
    pub nearby_string: Option<String>, // NEW
}
```

Indexing: when `kind == Dispatches` AND `nearby_string.is_some()`, add a secondary index `Map<String, Vec<(callee, edge)>>` keyed on `nearby_string`. This powers the `aft dispatches <key>` lookup in O(1) average.

Memory impact: the secondary index stores one entry per `dispatches` edge with a key. Expected scale: a few thousand entries per large service. Sub-megabyte.

### Existing commands behavior

- `aft callers` — today returns only edges with `kind: concrete | interface`. Extend to include `dispatches`, `goroutine`, `defer` by default, with a `--kind=<k1,k2,...>` filter to narrow. This makes "who calls X" answer "who causes X to run" by default, which is what users actually mean.
- `aft call_tree` — mirror: include all kinds by default, filter flag available.
- `aft trace_to` — include all kinds by default. An agent tracing "how does execution reach X" wants dispatches/defers/goroutines as legitimate paths.

Output format: add `"kind"` field to each edge in the JSON output (already partially there for interface vs concrete; extend enumeration).

## New commands

### `aft dispatched_by <file> <symbol>`

"Who passes this function as a value?" — reverse lookup on `dispatches` edges.

Input: symbol (and its file, for disambiguation).
Output: JSON list of edges with `kind == Dispatches` where the callee matches. Each edge includes the `nearby_string` key if present.

```json
{
  "symbol": "HandleMerchantSettlementV3Task",
  "file": "server/asynq_handler.go",
  "dispatched_by": [
    {
      "caller": {"file": "server/asynq_server.go", "symbol": "startAsyncQueueServer", "line": 48},
      "nearby_string": "TypeMerchantSettlementV3"
    }
  ]
}
```

Implementation: filter the reverse index for the given callee, keep only `kind == Dispatches`. Trivial on top of existing `callers` logic.

### `aft dispatches <key>`

"What handler is registered under this dispatch key?" — forward lookup on `nearby_string`.

Input: key string (exact match; `--prefix` flag for prefix match).
Output: JSON list of handlers whose dispatch edges carry this key.

```json
{
  "key": "TypeMerchantSettlementV3",
  "handlers": [
    {
      "handler": {"file": "server/asynq_handler.go", "symbol": "HandleMerchantSettlementV3Task"},
      "registered_by": {"file": "server/asynq_server.go", "symbol": "startAsyncQueueServer", "line": 48}
    }
  ]
}
```

Implementation: O(1) lookup in the secondary `nearby_string` index. With `--prefix`, linear scan of keys.

## Helper-side implementation sketch (Go)

`go-helper/main.go` gets a new visitor pass after the existing CHA-edge emission. Rough shape:

```go
// visit each SSA instruction in each function of each package
for _, pkg := range prog.AllPackages() {
    if !isInProjectPkg(pkg, root) { continue }
    for _, m := range pkg.Members {
        fn, ok := m.(*ssa.Function)
        if !ok { continue }
        for _, block := range fn.Blocks {
            for _, instr := range block.Instrs {
                switch v := instr.(type) {
                case *ssa.Call:
                    emitDispatchEdgesFromCall(fn, v, &out.Edges)
                case *ssa.Go:
                    emitGoroutineEdge(fn, v, &out.Edges)
                case *ssa.Defer:
                    emitDeferEdge(fn, v, &out.Edges)
                }
            }
        }
    }
}

func emitDispatchEdgesFromCall(caller *ssa.Function, call *ssa.Call, out *[]Edge) {
    strArg := singleStringArg(call.Call.Args, 128) // nil unless exactly one short string
    for _, arg := range call.Call.Args {
        target := resolveFuncValue(arg) // *ssa.Function or nil
        if target == nil { continue }
        if !isInProject(target, root) { continue }
        if target == caller { continue } // self-reference drop
        edge := Edge{
            Caller: callerPos(caller, call),
            Callee: calleeFrom(target),
            Kind:   "dispatches",
        }
        if strArg != nil {
            edge.NearbyString = strArg
        }
        *out = append(*out, edge)
    }
}
```

Helpers `resolveFuncValue`, `singleStringArg`, `isInProject`, `callerPos`, `calleeFrom` are either already present or near-copies of existing code.

**Deduplication:** the same `(caller, callee, kind, nearby_string)` tuple may arise multiple times (e.g. a handler registered from multiple call sites). Dedup at emit-time with a `Set` keyed on the tuple to keep the JSON small.

## Performance budget

Budget points, not nice-to-haves:

| Metric | Target | Rationale |
|---|---|---|
| Added helper runtime | < 20% over baseline | SSA walks are already done; we add per-instruction classification. Should be 5–15%. |
| Added JSON output size | < 30% over baseline | 2000–5000 extra dispatch edges per large service × ~200 bytes/edge ≈ 400KB–1MB. Target-service baseline ~1.5MB → new cap ~2MB. Well under pipe-deadlock (3MB) threshold. |
| Added Rust indexing time | < 10% over baseline | Secondary `nearby_string` index is a `HashMap<String, SmallVec>`; insertion is O(1) per dispatch edge. |
| Secondary-index memory | < 1MB per large project | 5000 entries × 200 bytes average = 1MB ceiling. |
| `aft dispatches <key>` latency | < 10ms after index warm | O(1) hash lookup. |

If any of these are missed, the implementation is not ready to ship. Benchmarks required (see Tests section).

## Rollout / feature flag

**Helper side:** CLI flag `-no-dispatches` disables emission of `dispatches`, `goroutine`, and `defer` edges. Default off (emissions on). Ops can revert to v1-equivalent output in one flag if needed.

**Rust side:** config knob `[callgraph] enable_dispatch_edges = true` in AFT's settings. Default `true`. When `false`, Rust drops any `dispatches`/`goroutine`/`defer` edges on parse (keeps v1-semantic reverse index). Useful for A/B comparison during rollout.

**Environment override:** `AFT_DISABLE_DISPATCH_EDGES=1` as a last-resort kill switch that sets both flags.

## Tests

### 1. Helper golden-file tests

`go-helper/testdata/dispatch/` with:

- `asynq.go` — asynq.ServeMux.HandleFunc("TypeX", handler)
- `http.go` — http.HandleFunc("/path", h), mux.Handle(pat, h)
- `grpc.go` — grpc.Server.Register*
- `gocron.go` — scheduler.Every(...).Do(handler)
- `goroutine.go` — `go f()` with various shapes
- `defer.go` — `defer f()` with various shapes
- `bad.go` — anonymous closures, multiple-string arg lists, out-of-project targets

Each fixture ships a `expected.json` with the exact set of dispatch edges the helper should emit. Test asserts equality after sort.

### 2. Rust-side deserialization tests

`crates/aft/src/go_helper.rs` unit tests:

- Old v1 output (no `nearby_string` field) round-trips unchanged.
- New output with all three new `kind` values parses correctly.
- `nearby_string` is correctly Some/None across edges.

### 3. Secondary-index tests

`crates/aft/src/callgraph.rs` tests:

- `aft dispatches <key>` finds the right handler.
- `aft dispatched_by <sym>` finds all dispatch sites.
- Keys with regex/special chars don't explode.

### 4. Benchmark

`benchmarks/dispatch_edges.rs` runs against fixtures (one small project, one medium, the real `example/target-service`) and records:

- Helper wall-clock (ratio vs baseline).
- JSON output size (ratio vs baseline).
- Rust ingestion time.
- Secondary-index memory (via `dhat` or similar).

Target: all within the budget table above. Fail the PR on regression.

## Open questions for the implementer

Each of these has a defensible default; the implementer may change it with a one-line rationale in the PR:

1. **MakeClosure with captured state:** `func() { doThing(x) }` where `x` is captured from the outer scope. Currently the design says "drop anonymous closures". Should we resolve to `doThing` (the function called inside the closure) when the closure has exactly one call? *Default: no — too speculative, agent would see a confusing chain.*

2. **Method-value receivers:** `t.Method` expression where `t` is a concrete type. SSA renders this as `MakeClosure` bound to the method. Do we emit the underlying `(T).Method` as the callee, or skip? *Default: emit; the receiver is in `callee.receiver`.*

3. **Multi-key dispatch:** `router.Get("/a", h).Get("/b", h)` — same handler under two keys. Emit two edges (one per key), or one edge with a joined key? *Default: two edges; each call-site is its own registration.*

4. **Interaction with interface edges:** a dispatch edge whose handler is itself an interface method. The CHA pass already emits interface edges for all implementations. Do we emit dispatch edges for each implementation too, or only for the interface method symbol? *Default: only for the interface method symbol — dispatch edge semantics are about the registration, CHA handles the subsequent call-site resolution.*

## Out of scope for this doc

Forwarded to later design docs, referenced here for traceability:

- **Pattern catalog / library-aware labels** (Tier 1.6) — pure post-processing of the emitted edges, does not affect this schema.
- **Persistent cross-session graph** (Tier 2) — storage / caching of these edges is covered by `DESIGN-persistent-graph.md`.
- **Semantic similarity / dispatch key fuzzy match** — `DESIGN-similarity.md`. The `aft dispatches <key>` command is exact+prefix-only in this doc.

## Summary

Add 1 optional field (`nearby_string`), 3 new `EdgeKind` variants, 2 new CLI commands, and one SSA visitor pass in the helper. Schema stays v1 (purely additive). Honors filter-at-source contract. Stays within a ≤30% JSON-size budget, ≤1MB secondary-index memory, ≤10ms query latency. Feature-flagged at both helper and Rust side for safe rollout.
