# ADR-0003: Variable/const nodes and write-site edges

## Status

Accepted — shipped in commit 43297f8.

## Context

`aft outline` surfaces functions, methods, types, and interfaces. Package-level `var` and `const` declarations are not treated as first-class symbols, so:

- `aft callers` cannot answer "who writes to `globalRegistry`?"
- `aft zoom` cannot jump to a const definition.
- Agents documenting flows must fall back to grep when asking "which file initializes `settlementConfig`?"

Go SSA tracks writes to package-level variables through the `*ssa.Global` type. This is unused.

The design is governed by the helper contract (`docs/helper-contract.md`):

1. **Two-layer implementation.** Tree-sitter surfaces declarations (cheap, all languages). The Go helper adds cross-package write-site edges (type-checked, language-specific).
2. **Filter at source.** Same-file reads/writes are not emitted — tree-sitter already indexes the declaration; reading the file answers local-usage questions. Only cross-package writes get helper edges.
3. **Not all vars/consts are equal.** Package-level `var`/`const` only — ignore function-local vars. Local-variable write tracking is what `aft trace_data` exists for.
4. **Additive. Schema stays v1.**

## Decision

### New tree-sitter symbol kind: `Variable` (and optionally `Constant`)

`aft outline` gains one new line per package-level declaration:

```
merchant_settlement/service.go
  type service struct                                 24
  var settlementStatusTerminal = []string{...}       45  # NEW
  const MaxBatchSize = 100                           52  # NEW
  func NewService(...) *service                      60
  func (s *service) InitiateMerchantSettlementV3(    3051
  ...
```

Implementation: tree-sitter's Go grammar already has `var_declaration` and `const_declaration` nodes at the top level. Extend the existing extractor in `crates/aft/src/calls.rs` (or wherever package-level outline walks live) to emit them with a new `SymbolKind::Variable` / `SymbolKind::Constant`.

### New `EdgeKind`: `writes`

A `writes` edge represents "function C assigns to package-level variable V":

```json
{
  "caller": {
    "file": "server/asynq_server.go",
    "line": 47,
    "symbol": "startAsyncQueueServer"
  },
  "callee": {
    "file": "server/registry.go",
    "symbol": "handlerRegistry",
    "pkg": "server"
  },
  "kind": "writes"
}
```

- `caller` = the function doing the write.
- `callee` = the package-level var being written (`symbol` = var name, not method name).

### Helper-side implementation

The helper walks SSA and looks for `*ssa.Store` instructions whose `Addr` operand is a `*ssa.Global`:

```go
for _, fn := range allProjectFunctions(prog, root) {
    for _, block := range fn.Blocks {
        for _, instr := range block.Instrs {
            store, ok := instr.(*ssa.Store)
            if !ok { continue }
            glob, ok := store.Addr.(*ssa.Global)
            if !ok { continue }
            if !isInProject(glob, root) { continue }

            // Filter: drop same-package writes — tree-sitter sees these.
            if glob.Pkg == fn.Pkg { continue }

            out.Edges = append(out.Edges, Edge{
                Caller: Caller{
                    File:   relPath(fn.Pos(), root),
                    Line:   store.Pos().Line,
                    Symbol: fn.Name(),
                },
                Callee: Callee{
                    File:   relPath(glob.Pos(), root),
                    Symbol: glob.Name(),
                    Pkg:    glob.Pkg.Path(),
                },
                Kind: "writes",
            })
        }
    }
}
```

**Why cross-package only:**
- Same-package writes: reading the package's files tells you everything. Emitting them balloons output for zero marginal value. Tree-sitter + `aft grep` handles same-package lookup faster than the helper ever will.
- Cross-package writes: genuinely hard to find without type info (e.g., `registry.Register = myRegistry` where `registry` is a different package).

**Dedup:** set keyed on `(caller_file, caller_line, glob_file, glob_symbol)`.

**Initialization writes (init functions, var declarations with function calls):** SSA represents these as stores in a synthetic `init` function. Treat `init` as a real caller — the edge is emitted with `caller.symbol = "init"`. Agents know what that means.

### Rust-side changes

#### `crates/aft/src/go_helper.rs`

Add to `EdgeKind`:
```rust
pub enum EdgeKind {
    // ... existing variants ...
    /// NEW: package-level variable write.
    Writes,
}
```

#### Tree-sitter side

In the outline extractor, add emission of `Variable` / `Constant` kinds. The existing data structures probably have a `SymbolKind` enum — extend it.

Impact on `aft outline` output: per-file symbol count goes up modestly (a Go service has dozens of top-level vars, not thousands). No performance concern.

#### Indexing

`writes` edges go into the same reverse index as call edges. Callers lookup by callee_symbol works naturally: `aft callers server/registry.go handlerRegistry` would return all writers (with `kind: writes`).

No secondary index needed — the existing callee-keyed index covers the lookup.

### New command

#### `aft writers <file> <variable_symbol>`

"Who writes to this package-level variable?"

Convenience alias over `aft callers --kind=writes`. Output:

```json
{
  "variable": {
    "file": "server/registry.go",
    "symbol": "handlerRegistry"
  },
  "writers": [
    {"file": "server/asynq_server.go", "symbol": "startAsyncQueueServer", "line": 47},
    {"file": "server/asynq_server.go", "symbol": "init", "line": 12}
  ]
}
```

Alias rationale: "writers" is what users will type. Agents will learn that `aft callers --kind=writes` is the general form.

### Extension to `aft callers` and `aft zoom`

- `aft callers` on a variable symbol returns `writes` edges (new kind, same command surface).
- `aft zoom` on a variable symbol returns the declaration + initializer + list of writers.

### Rollout / feature flag

- Helper: `-no-writes` flag disables `writes` edge emission.
- Rust: `[callgraph] enable_writes_edges = true`.
- Tree-sitter variable extraction is not flagged — it's cheap, safe, and orthogonal.

## Consequences

### Positive consequences

- `aft outline` now shows package-level vars and consts alongside functions and types — agents no longer need grep to discover them.
- `aft callers` / `aft writers` can answer "who writes to `globalRegistry`?" across package boundaries, which requires type info that tree-sitter doesn't have.
- `init`-function writes are surfaced correctly (SSA represents `var X = fn()` as a store from synthetic `init`).
- `writes` edges reuse the existing reverse index — no new data structure needed.
- Less than 10% added helper runtime; less than 5% added JSON output size (typical service: few hundred edges × ~200 bytes = <100KB).

### Trade-offs

- Only cross-package writes are emitted; same-package writes are filtered. Agents must use `aft grep` or read the file for same-package write sites.
- Only direct `*ssa.Store` with `*ssa.Global` as Addr — indirect writes through pointers are out of scope.
- Struct field writes on package-level struct vars (`GlobalCache.Set(k, v)`) are method calls, already covered by `concrete` edges. Map/slice element writes on globals are skipped.

### Open follow-ups

1. **Indirect writes:** `p := &GlobalVar; *p = 5`. SSA can trace this in some cases but not all. Currently out of scope (direct stores only).

2. **Struct field writes on package-level struct vars:** the first form (`GlobalCache.Set(k, v)`) is a method call and is already covered by `concrete` edges. The second form (`GlobalCache[k] = v`) is a `*ssa.IndexAddr` write — currently skipped.

3. **Write-kind sub-field** (assignment vs compound assign vs increment): currently collapsed to `writes` to keep the schema simple.
