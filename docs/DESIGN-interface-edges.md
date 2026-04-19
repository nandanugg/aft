# DESIGN — Interface implementation edges (Tier 1.4)

Status: design (not implemented)
Scope: one new `EdgeKind` variant, one new `aft` command, a small Go-helper pass that reuses existing CHA data.
Successor to: [`helper-contract.md`](helper-contract.md) and [`DESIGN-dispatch-edges.md`](DESIGN-dispatch-edges.md).

## Motivation

Today the helper emits `kind: "interface"` edges **at call sites** — one per resolved concrete implementation of the dispatched method. That answers "when `d.Do()` is called where `d Doer`, what actually runs." But it does *not* answer "which concrete types implement `Doer`" unless there's a call site; interfaces with no call sites in a given file produce no edges.

Agents asking architectural questions (*"what implements `SettlementStorer`?"*) get tree-sitter's best-effort (a grep for `func (... T) MethodName`) which misses interface satisfaction across packages, embedded interfaces, and structural typing edge cases.

Go's type checker + CHA already compute the full implements-relation. We just aren't emitting it.

## Design principles (binding)

1. **Filter at source.** Only emit implementations where both sides are in-project. Stdlib types implementing stdlib interfaces is noise at this scale.
2. **One edge per (interface_method, concrete_method) pair** — *not* per (interface, concrete_type). This matches the existing `interface`-kind edge granularity and lets `aft implementations` work by method name as well as by type.
3. **Independent of call sites.** The pass runs once per project against the CHA result, regardless of whether anything calls the interface method.
4. **Additive. Schema stays v1.** New `EdgeKind` variant only.

## Data model additions

### New `EdgeKind`: `implements`

An `implements` edge represents "concrete method M_C satisfies interface method M_I":

```json
{
  "caller": {
    "file": "settlement/store.go",
    "line": 34,
    "symbol": "SettlementStorer"
  },
  "callee": {
    "file": "store/settlement_store.go",
    "symbol": "Create",
    "receiver": "*pkg.settlementStore",
    "pkg": "store"
  },
  "kind": "implements"
}
```

**Field reuse (intentional overload):**
- `caller.file` + `caller.line` = interface method declaration site.
- `caller.symbol` = interface type name (not the method name — the method name is in `callee.symbol`, which matches the concrete method and is necessarily the same as the interface method by name).
- `callee` = concrete implementation (receiver type + method name + pkg).

This treats `implements` as a first-class edge in the existing shape rather than inventing a new node/edge type. A fully "proper" design would use nodes — we keep the flat edge list because the existing Rust-side reverse-index machinery already handles edges, and the value of this feature doesn't justify a new container.

### Why not bump `HelperEdge` with an `interface_method` field?

Tempting: `caller = interface type name, interface_method = "Create"`. But it departs from the existing "caller location is a file+line" convention and requires Rust-side special handling. The chosen encoding (caller.symbol = interface type, callee.symbol = method) keeps everything in the flat schema; the Rust side distinguishes via `kind == Implements`.

## Helper-side implementation sketch

`go-helper/main.go` gets a new pass after the existing CHA pass:

```go
for _, iface := range allInterfaceTypes(prog, root) {  // in-project only
    for _, method := range methodSetOf(iface) {
        impls := cha.ConcreteImplementations(prog, iface, method)
        for _, impl := range impls {
            if !isInProject(impl, root) { continue }
            if sameSiteAsInterface(impl, iface) { continue } // tree-sitter sees this
            out.Edges = append(out.Edges, Edge{
                Caller: Caller{
                    File:   relPath(iface.File(), root),
                    Line:   iface.MethodDecl(method).Line(),
                    Symbol: iface.Name(),
                },
                Callee: Callee{
                    File:     relPath(impl.File(), root),
                    Symbol:   impl.Name(),
                    Receiver: receiverTypeStr(impl),
                    Pkg:      impl.Pkg().Path(),
                },
                Kind: "implements",
            })
        }
    }
}
```

`cha.ConcreteImplementations` is a small wrapper over Go's `go/callgraph/cha` data — CHA already computed the implements-relation when resolving interface call sites; we just enumerate it directly.

**Deduplication:** set-keyed by `(interface_file, interface_line, iface_symbol, concrete_pkg, concrete_receiver, concrete_symbol)` to avoid double-emit when an interface is embedded.

**Filter: `sameSiteAsInterface`:**
- If the concrete implementation is in the same file as the interface declaration, tree-sitter will resolve it via plain call-site analysis. Skip — contract says drop what tree-sitter already knows.
- If the concrete is in the same package (different file, same directory) — keep, because Rust's current same-directory-same-package resolution only handles *call* edges, not *implements* edges.

## Rust-side changes

### `crates/aft/src/go_helper.rs`

Add the new variant:

```rust
pub enum EdgeKind {
    Interface,
    Concrete,
    Static,
    Dispatches,   // from DESIGN-dispatch-edges.md
    Goroutine,    // "
    Defer,        // "
    /// NEW: interface method → concrete implementation (satisfies-relation).
    Implements,
}
```

### `crates/aft/src/callgraph.rs`

Store `implements` edges in a **separate** data structure from call edges. Reason: call-edges are keyed on (callee_symbol, callee_file) with callers as values. Implements-edges are naturally keyed on interface type, with implementations as values — and queried by different commands.

```rust
pub struct ImplementationIndex {
    // Key: (interface_file, interface_symbol) -- interface type identity
    // Value: list of concrete implementations (from callee side of the edge)
    pub by_interface: HashMap<(PathBuf, String), Vec<ConcreteImpl>>,

    // Reverse: given a concrete type (receiver), what interfaces does it implement?
    // Key: receiver string like "*pkg.settlementStore"
    // Value: list of interfaces it satisfies
    pub by_concrete: HashMap<String, Vec<InterfaceRef>>,
}
```

Memory impact: one entry per `implements` edge. Typical Go service: a few dozen interfaces × a few implementations each → < 1000 entries. Well under 100KB.

## New command

### `aft implementations <file> <interface_symbol>`

"Which concrete types implement this interface?"

Input: the file containing the interface declaration, and the interface type name.
Output: list of concrete types + their method locations.

```json
{
  "interface": {
    "file": "settlement/store.go",
    "symbol": "SettlementStorer",
    "line": 34
  },
  "implementations": [
    {
      "receiver": "*store.settlementStore",
      "pkg": "store",
      "methods": [
        {"name": "Create", "file": "store/settlement_store.go", "line": 125},
        {"name": "FindOrCreate", "file": "store/settlement_store.go", "line": 501}
      ]
    },
    {
      "receiver": "*mocks.SettlementStorer",
      "pkg": "store/mocks",
      "methods": [...]
    }
  ]
}
```

Implementation: look up `ImplementationIndex.by_interface[(file, symbol)]`, group by `(receiver, pkg)`, collect all method-level edges.

**Exclude mocks by default:** add `--include-mocks` flag; default excludes `**/mocks/**` (directory) and `*Mock*` (receiver substring). Mocks almost always match by construction and drown the real implementations in the output.

### Extension to `aft callers`

When called on an interface method symbol, `aft callers` today returns only call sites. Add mode: if the query resolves to an interface method, also include the implementing concrete methods (so "who actually runs when X.Method() is called on interface X" works in one query).

Controlled by a flag: `aft callers --via-interface`. Default off to preserve existing behavior.

## Performance budget

| Metric | Target | Notes |
|---|---|---|
| Added helper runtime | < 5% | CHA already ran; we enumerate its existing output. |
| Added JSON output size | < 10% | Low count (< 1000 edges typical). |
| Implementation-index memory | < 100KB | Few hundred edges × 200 bytes. |
| `aft implementations` query | < 5ms | Hash lookup + sort. |

## Rollout / feature flag

- Helper: `-no-implements` flag disables emission.
- Rust: `[callgraph] enable_implementation_edges = true`.
- Default on; gate off for pure v1 behavior.

## Tests

1. **Helper golden-file tests**
   - `testdata/impls/` with embedded interfaces, cross-package implementations, struct + pointer receivers, generic constraints.
   - Assert edges emitted for all valid implementations, none for same-file-same-package.

2. **Rust deserialization**
   - `EdgeKind::Implements` round-trips.
   - Old outputs without `implements` edges parse unchanged.

3. **`aft implementations` tests**
   - Simple case: one interface, two implementations.
   - Embedded interface: `type Foo interface { Bar; Baz() }`.
   - Structural typing: `type Reader interface { Read(p []byte) (int, error) }` and a type with that method — expect the edge.
   - Mock exclusion.

4. **Benchmark**
   - Run against `example/target-service`. Must stay within budget.

## Open questions for the implementer

1. **Embedded interfaces:** `type Foo interface { Bar; Baz() }` where `Bar` is another interface. Do we enumerate `Bar`'s methods as `Foo`'s too? *Default: yes — a type implements `Foo` only if it implements `Bar` too, and agents asking "what implements Foo" want full coverage.*

2. **Generic types:** `type Container[T any] struct{...}` with methods. Does `Container[int]` vs `Container[string]` count as two implementations, or one? *Default: one — keyed on the generic declaration, not instantiations. SSA canonicalizes.*

3. **Empty interfaces (`any`/`interface{}`):** skip entirely — every type implements it, emission would explode the graph. *Default: skip.*

## Summary

One new edge kind (`implements`), one new CLI command (`aft implementations`), reuses existing CHA output. < 5% runtime overhead, < 100KB memory. Additive, schema stays v1.
