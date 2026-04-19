# DESIGN — Call-site provenance (dispatched_via + constant resolution)

Status: design (not implemented)
Scope: Go helper only. Rust side gets a passthrough field. No new commands.
Builds on: [`DESIGN-dispatch-edges.md`](DESIGN-dispatch-edges.md). Does not change `HELPER_SCHEMA_VERSION` — strictly additive fields.

## Motivation

The regression test on target-service showed a pattern: agents with access to `aft dispatches "merchant_settlement:merchant_id"` often described this string as "the asynq task type." It might be. It might also be a Redis lock key, a log tag, or a filename prefix. The dispatch edge doesn't tell the agent which — it only says "this string was adjacent to a function-value argument."

Two under-surfaced facts would let the agent tell:

1. **Which function received the function-value argument.** SSA sees this — it's the callee of the containing call site — but we don't emit it. Adding `dispatched_via: "github.com/hibiken/asynq.(*ServeMux).HandleFunc"` lets the agent reason about semantics without the helper needing a library catalog.

2. **What constant the `nearby_string` actually resolved from.** Real code often writes `asynq.HandleFunc(string(dpayAsynq.TypeMerchantSettlement), h)`. The first argument is a typed-constant type-cast. Our current `extractNearbyString` only matches bare string literals and misses this entirely — so `nearby_string` comes back empty for most real asynq registrations.

Both fixes are helper-side. Both are additive. Both stay within the filter-at-source contract.

## Design principles (binding)

1. **Emit structural provenance, don't interpret.** We add fields the agent can read; we don't classify them as "asynq task type" or "Redis key" — let the agent handle that.
2. **Library-agnostic.** No hardcoded list of dispatch libraries. The FQN is enough.
3. **Filter at source.** Resolve constants in the helper, not in Rust. The Rust side stores and returns what the helper gave it.
4. **Additive. Schema stays v1.**

## Feature 1 — `dispatched_via`

### New field on `HelperEdge`

Added only for `kind: "dispatches"` edges. Optional.

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub dispatched_via: Option<String>,
```

### What it contains

The fully-qualified name of the function whose call received the function-value argument. Format follows Go's `ssa.Function.String()`-style rendering:

- Free function: `"pkg/path.FuncName"` (e.g. `"github.com/go-redis/redis/v9.Set"`)
- Method on a value receiver: `"pkg/path.TypeName.Method"`
- Method on a pointer receiver: `"pkg/path.(*TypeName).Method"`
- Interface method call: `"(pkg/path.InterfaceName).Method"` (resolved type expr)
- Unresolvable callee (rare, usually when SSA didn't type-resolve): omit the field entirely — better silent than wrong.

### Go helper implementation

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

### Performance budget

- Per-site overhead: one type-switch and one string format. Negligible.
- JSON-size delta: ~60 bytes per dispatches edge on average. Target-service has ~15 dispatches edges today → < 1 KB added. Under budget.

## Feature 2 — Constant resolution in `extractNearbyString`

### What changes

Today:

```go
// extractNearbyString returns the single string literal ≤128 chars from args.
func extractNearbyString(args []ssa.Value) string {
    for _, arg := range args {
        c, ok := arg.(*ssa.Const)
        if !ok {
            continue
        }
        ...
    }
}
```

Missed case: `string(dpayAsynq.TypeMerchantSettlement)` compiles to an SSA `*ssa.ChangeType` or `*ssa.Convert` instruction whose operand is a `*ssa.Const`. The `*ssa.Const` IS a string constant — it's just one level of indirection away.

### Resolution rules (ordered, first match wins)

For each arg, dig through these wrappers until we find a `*ssa.Const` or give up:

1. **Direct constant**: `arg.(*ssa.Const)` with `constant.Kind() == constant.String` → use `constant.StringVal(c.Value)`.
2. **Type conversion wrapping a constant**: `arg.(*ssa.Convert).X` or `arg.(*ssa.ChangeType).X` → recurse one level, same rules.
3. **Named-type alias**: `type TaskType string; const X TaskType = "foo"` compiles to a `*ssa.Const` whose type is the named type, not `string`. Accept any `*ssa.Const` whose *underlying* type is `string` (`c.Type().Underlying() == types.Typ[types.String]`).
4. **Package-level string var**: `var topicName = "merchant.settlement"` compiles to a `*ssa.Global` read. If we see `*ssa.UnOp{Op: MUL, X: *ssa.Global}`, look at the global's initializer; if it's a compile-time string constant, use it. This is optional for v1 (package vars are rare dispatch keys in practice); can ship later.

### Bound: max recursion depth 3

If we haven't found a constant after 3 unwrap hops, give up. Prevents pathological walking through chains of conversions.

### Same length cap (128 chars) and same "exactly one" rule

The policy from `DESIGN-dispatch-edges.md §1.2` is unchanged: if multiple string constants show up across args, drop nearby_string (ambiguous). Applying the resolution rule doesn't change the policy — only the set of things that *count* as a string constant.

### Go helper implementation

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

### Performance budget

- Per-arg overhead: ≤ 3 hop type-switch. O(1) in practice.
- No new JSON bytes (same field, just populated in more cases).
- Expected effect on target-service: `nearby_string` populated on all 12 asynq registrations (currently only 1). Semantic accuracy of dispatch-docs should improve materially.

## Rust-side changes

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

## Rollout / feature flag

No new flag. These are additive; agents that ignore `dispatched_via` get identical-to-today behavior. Constant resolution is a pure bug-fix relative to `extractNearbyString`'s intent.

## Tests

### Go helper golden

Add fixtures to `go-helper/testdata/dispatch/`:

- `asynq_typed_const.go` — `asynq.HandleFunc(string(TypeXxx), handler)` where `TypeXxx` is a named-type string constant. Expected: `nearby_string = "xxx.actual.value"`, `dispatched_via = "...(*ServeMux).HandleFunc"`.
- `interface_dispatch_site.go` — a dispatch via an interface-method receiver. Expected: `dispatched_via = "(iface.Iface).Method"`.
- `no_string_const.go` — dispatch site with no string literal anywhere. Expected: `nearby_string` absent, `dispatched_via` still present.

Update `expected.json` in the existing dispatch fixture directory to reflect the new field.

### Rust deserialization

Old helper output (no `dispatched_via`) must round-trip. New output with the field populated must deserialize cleanly.

### Command-level

Add an integration test: `aft dispatched_by <handler>` returns an edge with `dispatched_via` populated to the expected FQN.

## Open questions for the implementer

1. **Interface-receiver rendering.** For a call like `receiver.DoThing(handler)` where `receiver` is of interface type `Dispatcher`, the FQN could be `"pkg.Dispatcher.DoThing"` or `"(pkg.Dispatcher).DoThing"`. *Default: use the second form for consistency with Go's SSA rendering (parenthesized interface types).*

2. **Stdlib dispatched_via.** For calls into stdlib (`http.HandleFunc`, `sort.Slice`), should we elide the package path? Sort.Slice shows up as a callback target — is it noise? *Default: keep the FQN. An agent seeing `sort.Slice` can interpret "this isn't real dispatch" just fine.*

3. **Package-var constant (feature 2 rule 4).** Implement in v1 or defer? *Default: defer. Rare in the wild; ship a v2 if we see it matter.*

## Summary

Two small helper-side additions that give the agent the two missing facts driving most dispatch-edge misinterpretation: **who received the function value** (`dispatched_via`) and **what was the literal string** (constant-resolved `nearby_string`). ~120 LOC Go, ~50 LOC Rust plumbing, no new commands. Schema stays v1. Expected large accuracy lift on modified/universal dispatch-related claims; verify via regression rerun.
