# ADR-0002: Interface implementation edges

## Status

Accepted — shipped in commit 1a4834e.

## Context

The helper emits `kind: "interface"` edges **at call sites** — one per resolved concrete implementation of the dispatched method. That answers "when `d.Do()` is called where `d Doer`, what actually runs." But it does *not* answer "which concrete types implement `Doer`" unless there's a call site; interfaces with no call sites in a given file produce no edges.

Agents asking architectural questions (*"what implements `SettlementStorer`?"*) get tree-sitter's best-effort (a grep for `func (... T) MethodName`) which misses interface satisfaction across packages, embedded interfaces, and structural typing edge cases.

Go's type checker + CHA already compute the full implements-relation. The data is there; we just aren't emitting it.

Constraints from the helper contract (`docs/helper-contract.md`):

1. **Filter at source.** Only emit implementations where both sides are in-project. Stdlib types implementing stdlib interfaces is noise at this scale.
2. **One edge per (interface_method, concrete_method) pair** — *not* per (interface, concrete_type). This matches the existing `interface`-kind edge granularity and lets `aft implementations` work by method name as well as by type.
3. **Independent of call sites.** The pass runs once per project against the CHA result, regardless of whether anything calls the interface method.
4. **Additive. Schema stays v1.** New `EdgeKind` variant only.

## Decision

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

**Why not bump `HelperEdge` with an `interface_method` field?** Tempting: `caller = interface type name, interface_method = "Create"`. But it departs from the existing "caller location is a file+line" convention and requires Rust-side special handling. The chosen encoding (caller.symbol = interface type, callee.symbol = method) keeps everything in the flat schema; the Rust side distinguishes via `kind == Implements`.

### Helper-side implementation

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

### Rust-side changes

#### `crates/aft/src/go_helper.rs`

Add the new variant:

```rust
pub enum EdgeKind {
    Interface,
    Concrete,
    Static,
    Dispatches,   // from ADR-0001-dispatch-edges.md
    Goroutine,    // "
    Defer,        // "
    /// NEW: interface method → concrete implementation (satisfies-relation).
    Implements,
}
```

#### `crates/aft/src/callgraph.rs`

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

### New command

#### `aft implementations <file> <interface_symbol>`

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

### Rollout / feature flag

- Helper: `-no-implements` flag disables emission.
- Rust: `[callgraph] enable_implementation_edges = true`.
- Default on; gate off for pure v1 behavior.

## Consequences

### Positive consequences

- Agents asking "what implements `SettlementStorer`?" now get a complete answer across all packages, not just grep results.
- Works for embedded interfaces and cross-package implementations — cases tree-sitter misses.
- `ImplementationIndex` stays under 100KB for typical Go services.
- Less than 5% added helper runtime (CHA already ran; we enumerate its existing output).
- Less than 10% added JSON output size (< 1000 edges typical).

### Trade-offs

- `implements` edges reuse the flat `HelperEdge` shape with an intentional field-overload (`caller.symbol` = interface type, not a call site). This is a semantic quirk that consumers must understand when dispatching on `kind`.
- Empty interfaces (`any`/`interface{}`) are skipped — every type implements it, emission would explode the graph.
- The `ImplementationIndex` is a separate structure from call-edge indexes, requiring separate lookup paths.

### Open follow-ups

1. **Embedded interfaces:** `type Foo interface { Bar; Baz() }` where `Bar` is another interface. Currently implemented to enumerate `Bar`'s methods as `Foo`'s too — a type implements `Foo` only if it implements `Bar` too.

2. **Generic types:** `type Container[T any] struct{...}` with methods. Currently keyed on the generic declaration, not instantiations. SSA canonicalizes.

3. **Empty interfaces (`any`/`interface{}`):** explicitly skipped.

## Alternatives considered

**Separate "implements" node type** rather than reusing the flat edge shape was considered. Rejected because the existing Rust-side reverse-index machinery already handles edges, and the query patterns are sufficiently similar. A node-based design would require new containers and new plumbing for marginal expressive gain.

**Per-(interface, concrete_type) edges** rather than per-(interface_method, concrete_method) were considered. Rejected because the existing `interface`-kind edge granularity is per-method, and consistency makes the `aft implementations` command able to work by method name as well as by type.
