# ADR-0006: Call-site provenance (dispatched_via and constant resolution)

## Status

Accepted — shipped in commit 0d6d73f.

## Context

Regression testing on target-service revealed a pattern: agents with access to `aft dispatches "merchant_settlement:merchant_id"` often described this string as "the asynq task type." It might be. It might also be a Redis lock key, a log tag, or a filename prefix. The dispatch edge (per ADR-0001-dispatch-edges.md) doesn't tell the agent which — it only says "this string was adjacent to a function-value argument."

Two under-surfaced facts would let the agent tell:

1. **Which function received the function-value argument.** SSA sees this — it's the callee of the containing call site — but we don't emit it. Adding `dispatched_via: "github.com/hibiken/asynq.(*ServeMux).HandleFunc"` lets the agent reason about semantics without the helper needing a library catalog.

2. **What constant the `nearby_string` actually resolved from.** Real code often writes `asynq.HandleFunc(string(dpayAsynq.TypeMerchantSettlement), h)`. The first argument is a typed-constant type-cast. The original `extractNearbyString` only matches bare string literals and misses this entirely — so `nearby_string` comes back empty for most real asynq registrations.

Both fixes are helper-side. Both are additive. Both stay within the filter-at-source contract (`docs/helper-contract.md`).

Binding design principles:

1. **Emit structural provenance, don't interpret.** We add fields the agent can read; we don't classify them as "asynq task type" or "Redis key" — let the agent handle that.
2. **Library-agnostic.** No hardcoded list of dispatch libraries. The FQN is enough.
3. **Filter at source.** Resolve constants in the helper, not in Rust. The Rust side stores and returns what the helper gave it.
4. **Additive. Schema stays v1.**

## Decision

### Feature 1: `dispatched_via`

#### New field on `HelperEdge`

Added only for `kind: "dispatches"` edges. Optional.

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub dispatched_via: Option<String>,
```

#### What it contains

The fully-qualified name of the function whose call received the function-value argument. Format follows Go's `ssa.Function.String()`-style rendering:

- Free function: `"pkg/path.FuncName"` (e.g. `"github.com/go-redis/redis/v9.Set"`)
- Method on a value receiver: `"pkg/path.TypeName.Method"`
- Method on a pointer receiver: `"pkg/path.(*TypeName).Method"`
- Interface method call: `"(pkg/path.InterfaceName).Method"` (resolved type expr)
- Unresolvable callee (rare, usually when SSA didn't type-resolve): omit the field entirely — better silent than wrong.

#### Go helper implementation

In `emitDispatchesFromCall`, we already have `site ssa.CallInstruction`. Its `Common().Value` is the callee:

```go
func calleeFQN(common *ssa.CallCommon) (string, bool) {
    // Direct function call: value is *ssa.Function
    if fn, ok := common.Value.(*ssa.Function); ok {
        return fn.String(), true  // e.g. "github.com/hibiken/asynq.(*ServeMux).HandleFunc"
    }
    // Interface method call: IsInvoke() && Method != nil
    if common.IsInvoke() && common.Method != nil {
        recv := common.Value.Type().String()
        return fmt.Sprintf("(%s).%s", recv, common.Method.Name()), true
    }
    // Method call via bound method closure
    if mc, ok := common.Value.(*ssa.MakeClosure); ok {
        if fn, ok := mc.Fn.(*ssa.Function); ok {
            return fn.String(), true
        }
    }
    return "", false
}
```

Call this once per site in `emitDispatchesFromCall`, set on every dispatch edge we emit from that site.

#### Performance budget

- Per-site overhead: one type-switch and one string format. Negligible.
- JSON-size delta: ~60 bytes per dispatches edge on average. Target-service has ~15 dispatches edges today → < 1 KB added. Under budget.

### Feature 2: Constant resolution in `extractNearbyString`

#### What changes

Today `extractNearbyString` only matched `*ssa.Const` directly. This misses: `string(dpayAsynq.TypeMerchantSettlement)` compiles to an SSA `*ssa.ChangeType` or `*ssa.Convert` instruction whose operand is a `*ssa.Const`. The `*ssa.Const` IS a string constant — it's just one level of indirection away.

#### Resolution rules (ordered, first match wins)

For each arg, dig through these wrappers until we find a `*ssa.Const` or give up:

1. **Direct constant**: `arg.(*ssa.Const)` with `constant.Kind() == constant.String` → use `constant.StringVal(c.Value)`.
2. **Type conversion wrapping a constant**: `arg.(*ssa.Convert).X` or `arg.(*ssa.ChangeType).X` → recurse one level, same rules.
3. **Named-type alias**: `type TaskType string; const X TaskType = "foo"` compiles to a `*ssa.Const` whose type is the named type, not `string`. Accept any `*ssa.Const` whose *underlying* type is `string` (`c.Type().Underlying() == types.Typ[types.String]`).
4. **Package-level string var**: `var topicName = "merchant.settlement"` compiles to a `*ssa.Global` read. If we see `*ssa.UnOp{Op: MUL, X: *ssa.Global}`, look at the global's initializer; if it's a compile-time string constant, use it. This is deferred (package vars are rare dispatch keys in practice).

#### Bound: max recursion depth 3

If we haven't found a constant after 3 unwrap hops, give up. Prevents pathological walking through chains of conversions.

#### Same length cap (128 chars) and same "exactly one" rule

The policy from ADR-0001-dispatch-edges.md (the "nearby_string" section) is unchanged: if multiple string constants show up across args, drop nearby_string (ambiguous). Applying the resolution rule doesn't change the policy — only the set of things that *count* as a string constant.

#### Go helper implementation

Replace `extractNearbyString` with a helper that uses the resolution chain:

```go
func resolveStringConst(val ssa.Value, depth int) (string, bool) {
    if depth > 3 {
        return "", false
    }
    switch v := val.(type) {
    case *ssa.Const:
        if v.Value == nil || v.Value.Kind() != constant.String {
            return "", false
        }
        if u := v.Type().Underlying(); u != types.Typ[types.String] {
            // Named-type alias — still accept if underlying is string.
            if _, ok := u.(*types.Basic); !ok || u.(*types.Basic).Kind() != types.String {
                return "", false
            }
        }
        return constant.StringVal(v.Value), true
    case *ssa.Convert:
        return resolveStringConst(v.X, depth+1)
    case *ssa.ChangeType:
        return resolveStringConst(v.X, depth+1)
    }
    return "", false
}

func extractNearbyString(args []ssa.Value) string {
    var found string
    count := 0
    for _, arg := range args {
        s, ok := resolveStringConst(arg, 0)
        if !ok {
            continue
        }
        if len(s) > 128 {
            continue
        }
        count++
        found = s
    }
    if count != 1 {
        return ""
    }
    return found
}
```

#### Performance budget

- Per-arg overhead: ≤ 3 hop type-switch. O(1) in practice.
- No new JSON bytes (same field, just populated in more cases).
- Expected effect on target-service: `nearby_string` populated on all 12 asynq registrations (previously only 1). Semantic accuracy of dispatch-docs should improve materially.

### Rust-side changes

One-field addition to `HelperEdge`:

```rust
pub struct HelperEdge {
    // ... existing fields ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatched_via: Option<String>,
}
```

Propagate through `IndexedCallerSite` and the cache's `CallerSite` mirror type (same pattern as `kind`, `nearby_string`). Plumb into the output of `aft dispatched_by` and `aft dispatches`:

```json
{
  "key": "merchant_settlement:merchant_id",
  "handlers": [
    {
      "handler": {"file": "server/asynq_handler.go", "symbol": "HandleMerchantSettlementTask"},
      "registered_by": {"file": "server/asynq_server.go", "symbol": "startAsyncQueueServer", "line": 69},
      "dispatched_via": "github.com/hibiken/asynq.(*ServeMux).HandleFunc"
    }
  ]
}
```

Text renderer: append ` via {dispatched_via}` when present.

### Rollout / feature flag

No new flag. These are additive; agents that ignore `dispatched_via` get identical-to-today behavior. Constant resolution is a pure bug-fix relative to `extractNearbyString`'s intent.

## Consequences

### Positive consequences

- `dispatched_via` gives the agent the FQN of the registering function, letting it distinguish `asynq.HandleFunc` from `redis.Set` or `logger.With` without a library catalog.
- Constant resolution in `extractNearbyString` populates `nearby_string` for typed-constant dispatch keys (the dominant pattern in asynq, Kafka consumers, etc.). Expected to recover `nearby_string` on 11 of 12 registrations that were previously empty on target-service.
- Both changes are additive — old helper output (no `dispatched_via`) continues to round-trip unchanged.
- Schema stays v1; no helper schema version bump.
- ~120 LOC Go, ~50 LOC Rust plumbing.

### Trade-offs

- `dispatched_via` is absent when the callee SSA type is unresolvable (rare: dynamic dispatch through reflection or computed function values). Callers must handle the absent field.
- Package-level string var resolution (rule 4) is deferred. Rare in practice; the three constant-unwrap rules cover the vast majority of real dispatch registrations.
- Interface-receiver FQN uses parenthesized form `"(pkg.Dispatcher).DoThing"` for consistency with Go's SSA rendering — not everyone is familiar with this convention.

### Open follow-ups

1. **Interface-receiver rendering.** For a call where `receiver` is of interface type `Dispatcher`, the FQN uses `"(pkg.Dispatcher).DoThing"`. This is consistent with Go's SSA rendering but may be unfamiliar. A future iteration could provide a human-friendlier rendering.

2. **Stdlib dispatched_via.** For calls into stdlib (`http.HandleFunc`, `sort.Slice`), the full FQN is emitted. An agent seeing `sort.Slice` can interpret "this isn't real dispatch" just fine, but a future filter flag could omit stdlib entries.

3. **Package-var constant (rule 4).** `var topicName = "merchant.settlement"` as a dispatch key — resolve to its initializer. Deferred; rare in the wild.
