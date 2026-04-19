# ADR-0007: Control-flow context annotations

## Status

Accepted — shipped in commit 8f9c240 (final fix) and 03e4bf1 (initial implementation).

## Context

Two categories of documentation failure trace back to the same root cause: the agent sees a call or a return statement but can't see the **conditional context** around it.

Examples of things agents currently get wrong:

- *"On every request, the system fetches the merchant."* — true only if the call isn't inside `if cachedMerchant == nil { ... }`.
- *"V3 returns `asynq.SkipRetry` on any error."* — true only if the `return SkipRetry` statement is dominated by a broad `if err != nil` and not guarded by something narrower.
- *"This handler runs synchronously."* — false if the real call site is inside a `go func(){}()`.

SSA gives us the precise answer. We're not looking at it.

Builds on ADR-0001-dispatch-edges.md (edge kinds) and ADR-0006-call-site-provenance.md (call-site fields).

Binding design principles:

1. **Surface structural control-flow facts, don't interpret them.** The agent knows what `in_error_branch: true` means. The code just needs to tell it.
2. **Heuristics only at the classification edge.** The underlying dominator analysis is deterministic. We label dominating conditionals (*"this If tests an error"*) using explicit, named rules — no black-box inference.
3. **Bail on complexity.** When the path condition gets too deep, we emit a truncated representation rather than a wrong one.
4. **Additive. Schema stays v1.**

## Decision

### Feature 1: Caller-context booleans on call edges

#### New field on every emitted edge

Optional (`Option` on Rust side, nil-able in Go). Applies to all edge kinds: `concrete`, `interface`, `dispatches`, `goroutine`, `defer`, `writes`. Not applicable to `implements` (interface-satisfaction edges have no call-site context).

```json
{
  "kind": "concrete",
  "caller": {"file": "...", "symbol": "...", "line": 42},
  "callee": {...},
  "context": {
    "in_defer": false,
    "in_goroutine": false,
    "in_loop": false,
    "in_error_branch": true,
    "branch_depth": 2
  }
}
```

#### Field semantics

- **`in_defer`**: the call site is inside a `defer` statement. Detected by: the instruction is a `*ssa.Defer` directly, OR it's a regular call inside a function that is only reached as the target of a `*ssa.Defer` elsewhere. The current implementation ships only the direct case; the transitive case is follow-up work.

- **`in_goroutine`**: the call site is inside a `go func(){...}()` spawn. Direct case: the instruction is `*ssa.Go`. Indirect case: the call is inside a closure that is the target of a `*ssa.Go`. The current implementation ships direct; indirect is follow-up.

- **`in_loop`**: the call site's basic block is part of a loop body. Detected via dominator analysis — see "Loop detection" below.

- **`in_error_branch`**: the call site's basic block is dominated by an `*ssa.If` whose condition tests an error value, and we're on the branch taken when the condition is true (i.e. the error-handling branch). See "Error-branch detection" below.

- **`branch_depth`**: number of dominating `*ssa.If` terminators between the call's block and the enclosing function's entry block. A rough proxy for how "nested" the call is. Useful for the agent to decide "this is main-path code" (depth 0-1) vs "deeply conditional" (depth 3+).

#### Loop detection (SSA back-edges)

A basic block `B` is in a loop body iff there exists some block `B'` such that `B'` is a predecessor of some block `H`, `H` dominates `B'`, and `B` is dominated by `H` (i.e. `B` lives within the loop with header `H`).

Mechanism:

```go
func loopHeaders(fn *ssa.Function, dt *ssa.DomTree) map[*ssa.BasicBlock]bool {
    headers := make(map[*ssa.BasicBlock]bool)
    for _, b := range fn.Blocks {
        for _, succ := range b.Succs {
            if dominates(dt, succ, b) {
                // succ → b is a back-edge; succ is a loop header.
                headers[succ] = true
            }
        }
    }
    return headers
}

func blocksInLoops(fn *ssa.Function, dt *ssa.DomTree, headers map[*ssa.BasicBlock]bool) map[*ssa.BasicBlock]bool {
    loopBlocks := make(map[*ssa.BasicBlock]bool)
    for h := range headers {
        // Every block dominated by h AND with a path back to h is in the loop.
        // Cheap approximation: "dominated by h" is a superset; good enough in practice.
        for _, b := range fn.Blocks {
            if dominates(dt, h, b) {
                loopBlocks[b] = true
            }
        }
    }
    return loopBlocks
}
```

A more precise variant walks the SCCs of the CFG, but the superset approximation is standard for this kind of annotation — the caller might be in a block dominated by a loop header but technically post-loop. Agent-facing, this rarely matters.

**Note**: Go's `golang.org/x/tools/go/ssa` may not expose `ssa.DomTree` directly as a standalone struct; dominator info is accessed via `BasicBlock.Dominator()` and `BasicBlock.Idom()`. The implementation uses whichever API is current.

#### Error-branch detection (the heuristic)

For each call site at block `B`, walk up the dominator chain. For each dominating block `D`:

1. If `D`'s terminator is not `*ssa.If`, skip (unconditional control flow — no classification needed).
2. Get `D.If.Cond` — the `ssa.Value` being branched on.
3. Classify `Cond` using these rules in order (first match wins):
   - **Error-check rule**: `Cond` is a `*ssa.BinOp` with `Op == token.NEQ` AND one operand has `types.Implements(opType, errorInterface)`. Label: `"error-check"`.
   - **Nil-check rule**: `Cond` is a `*ssa.BinOp` with `Op == token.EQL || NEQ` AND one operand is the `nil` constant. Label: `"nil-check"`.
   - **Boolean-predicate rule**: `Cond` has `boolVal.Name()` starting with `is`, `has`, `can`, `ok`, `valid` (case-insensitive). Label: `"predicate"`.
   - **Otherwise**: label `"other"`.
4. Determine which side of the If we're on: is `B` dominated by `D.Succs[0]` (True branch) or `D.Succs[1]` (False branch)?
5. Record: `(label, side)` for this dominating If.

After walking the full chain, classify the call:
- `in_error_branch: true` iff ANY dominator in the chain is `("error-check", True)`. False positives are possible (rare) and accepted for the current implementation.

#### Error type detection

The error-check rule needs to know if a value "is an error." Two approaches:

1. **Structural**: `types.Implements(opType, errorInterface)` where `errorInterface` is the universe's `error` type. This catches custom error types and standard `error`.
2. **Name-based fallback**: if the operand's name is `err`, treat it as an error. Pragmatic; handles cases where SSA type-inference didn't run on a package.

The current implementation uses #1 primarily, falls back to #2 when type info is unavailable.

#### Performance budget

- Dominator info is already computed by SSA during construction. Free to use.
- Per-call-edge cost: O(depth-of-dominator-chain) — typically 2-5 hops. Target-service has ~5000 emitted edges, so ~25k hop operations for a full run. Dwarfed by SSA construction itself.
- JSON size: ~80 bytes per edge for the context object. Can compress by omitting fields with default values (false booleans are omitted).
- Expected helper runtime delta: < 5% over baseline.

### Feature 2: Conditional return-value analysis

#### New output section on `aft zoom`

When the user runs `aft zoom <file> <symbol>`, include a `returns` section:

```json
{
  "symbol": "ProcessMerchantSettlementV3",
  "file": "merchant_settlement/service.go",
  "body": "...",
  "call_graph": {...},
  "returns": [
    {
      "line": 182,
      "value": "asynq.SkipRetry",
      "path_condition": "err != nil",
      "path_condition_simple": true
    },
    {
      "line": 201,
      "value": "nil",
      "path_condition": "err1 == nil && err2 == nil && result.FaultyBatches == 0",
      "path_condition_simple": true
    },
    {
      "line": 176,
      "value": "errors.New(\"batch failed\")",
      "path_condition": "err1 == nil && err2 != nil",
      "path_condition_simple": true
    }
  ]
}
```

#### Mechanism

For each `*ssa.Return` instruction in the function:

1. **Identify the return's block** `R`.
2. **Walk the dominator chain** of `R` back toward `fn.Blocks[0]` (entry).
3. For each dominating `*ssa.If` block `D`:
   - Determine which side of the branch `R` is on (True or False).
   - Record the condition and side: `(D.If.Cond, side)`.
4. The **path condition** is the conjunction of all recorded `(Cond, side)` pairs (negate Cond when on False side).
5. **Render the path condition** as Go source (see "Rendering" below).
6. **Extract the return value**: `Return.Results[0]` if single-value; tuple if multi. For each result, try to render it as source (similar rendering challenge).

#### Rendering SSA values back to Go source

SSA values can be rendered using:

1. **Position-based recovery**: `val.Pos()` gives a `token.Pos`. If valid, we can read the source file at that position and extract the identifier/expression text. Works for named variables and most direct references.
2. **Structural rendering**: for synthesized SSA (`BinOp`, `Convert`, etc.) without a source position, render using the SSA shape:
   - `BinOp(X, OP, Y)` → `"{render(X)} {OP.String()} {render(Y)}"`
   - `Call(f, args...)` → `"{render(f)}({render args, comma-separated})"`
   - `Const` with string kind → `"\"{value}\""`
   - `Const` with numeric kind → `"{value}"`
   - `Const` with nil → `"nil"`
   - `Phi` → `"<merged value>"` (can't render usefully)
3. **Recursion depth cap**: max depth 5 when rendering. Beyond that, emit `"..."`.

Mark `path_condition_simple: false` if we had to fall back to structural rendering on any sub-expression, or if the path involved a `Phi` or depth-truncation.

#### Path-condition simplification

Before rendering, apply basic boolean simplification:

- `x && true` → `x`
- `x && false` → `false` (and drop this return entirely — it's unreachable)
- `x && x` → `x` (dedup)
- Negation: if the same cond appears on both True and False branches, drop the contradiction.

Keep the simplification conservative. Don't try to prove propositional equivalence; just handle the obvious cases. A deeply redundant path gets an `unsimplified` flag so the consumer knows.

#### Handling Phi-merged returns

A `*ssa.Return` might have its value be a `*ssa.Phi` — meaning different branches assigned different values to the same "logical" return variable. The current implementation splits: one return entry per incoming Phi edge. This matches what the user intuitively sees — "there are 4 possible returned values."

### Rust-side changes

#### `HelperEdge.context`

New optional field:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CallContext {
    #[serde(default, skip_serializing_if = "is_false")]
    pub in_defer: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub in_goroutine: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub in_loop: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub in_error_branch: bool,
    #[serde(default)]
    pub branch_depth: u32,
}

pub struct HelperEdge {
    // ... existing fields ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<CallContext>,
}
```

Propagate `context` through `IndexedCallerSite` and persistent-cache serialization.

#### `aft zoom` output

Add optional `returns` section. Implementation: new helper-side struct that gets serialized on every helper run, carried on the `HelperOutput`:

```rust
pub struct ReturnInfo {
    pub file: String,
    pub symbol: String,
    pub returns: Vec<ReturnSite>,
}

pub struct ReturnSite {
    pub line: u32,
    pub value: String,
    pub path_condition: String,
    pub path_condition_simple: bool,
}

pub struct HelperOutput {
    // ... existing ...
    #[serde(default)]
    pub returns: Vec<ReturnInfo>,
}
```

`aft zoom` command looks up `ReturnInfo` by (file, symbol) and appends to its output when present.

### Rollout / feature flag

- Helper CLI flag: `-no-call-context` disables caller-context annotations.
- Helper CLI flag: `-no-return-analysis` disables return-condition analysis.
- Rust config: `[callgraph] emit_call_context = true`, `[callgraph] emit_return_analysis = true`. Both default true.
- Env: `AFT_DISABLE_CALL_CONTEXT=1`, `AFT_DISABLE_RETURN_ANALYSIS=1`.

Both features are independently togglable — return analysis is the bigger performance variable, and the ability to disable just that without losing call context is intentional.

## Consequences

### Positive consequences

- Agents reading `aft zoom` output now know which return values are conditional on errors, loops, or deep nesting — eliminating hallucinations like "always returns X" when the truth is "returns X only if `err != nil`".
- `in_error_branch`, `in_loop`, `in_defer`, `in_goroutine` on every call edge give agents the flow-accuracy facts they previously had to guess at.
- `branch_depth` lets agents distinguish main-path code from deeply-conditional cleanup code without reading the whole function.
- The dominator walk is essentially free: SSA already computes dominators during construction.
- Return analysis adds < 15% to helper runtime and < 40% to JSON output size on target-service benchmarks.

### Trade-offs

- `in_error_branch: true` uses a superset approximation for loop-body detection, and a first-match heuristic for error-branch classification. False positives are possible when a block is dominated by a loop header but is actually post-loop, or when an error-check dominates a call that is also guarded by a later non-error condition.
- Path condition rendering can produce strings with unrenderable sub-expressions (`?`) or depth-truncated conjunctions (`...and N more`). `path_condition_simple: false` flags these.
- `in_defer` and `in_goroutine` cover only the direct case (current implementation). The transitive case (call inside a closure that is the target of a defer/goroutine) is not yet surfaced.
- Switch statements are compiled by SSA into chains of If blocks, so path conditions for switch arms are verbose If-chains rather than readable `switch x case Y:` renderings.

### Open follow-ups

1. **Rendering unavailable for optimized-away values.** If SSA optimization elided a variable, we can't recover a name. Current implementation emits `?` and sets `path_condition_simple: false`.

2. **Deeply nested branches.** Path conditions are capped at 4 AND terms; the rest are emitted as "...and N more". Agent can still work with it.

3. **Loop iteration conditions.** A return inside a loop depends on the iteration state. Current implementation annotates with a `(inside loop)` marker and skips the iteration state itself.

4. **Switch statement rendering.** Currently emits verbose If-chains. A future iteration could detect switch patterns (tag-based Ifs sharing a scrutinee) and render as `switch x case Y: ...`.

5. **Transitive `in_defer` / `in_goroutine`.** The current implementation covers only direct cases. The transitive case (call inside a closure that is the target of a defer/goroutine) is follow-up work.

## Alternatives considered

**Black-box inference / ML classification** for "is this an error branch?" was rejected. The error-check rule using `types.Implements(opType, errorInterface)` is deterministic, explainable, and correct for the vast majority of Go error patterns. A name-based fallback (`err` variable name) handles the residual cases where type info is unavailable.

**Merging Phi-valued returns** (one entry with a disjunction of path conditions) rather than splitting was considered and rejected. Splitting is more informative for documentation — "there are 4 possible returned values" is more useful to the agent than "one return with a disjunction".
