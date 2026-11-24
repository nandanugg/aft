# go-facts: expectations taken from the old aft fork Go helper tests

Source: fork branch backup/main-before-origin-rebase-20260705 (6bd5b9f47), tests in go-helper/*_test.go.
Flow tests come from feat/go-vta-helper (1c33802fb) and feat/trace-data-patterns (beb0929cb); the backup branch has the fixture data_flow_patterns.go but not those six Rust tests.
Fixtures in testdata/ are byte copies. Positions in expected.json are 1-based, col in UTF-8 bytes, relative to each module root.

- testdata/controlflow: defer, go, loop, error branch, nested branches, return paths. 5 site_tags, 3 returns.
- testdata/dispatch: registration calls (asynq, http, typed consts, closures, interface), goroutine, defer, interface field dispatch. 25 registers (13 golden rows with exact via, 10 per-test rows, 1 count, 1 absent), 6 site (3 goroutine/defer, 2 interface field, 1 count), 3 site_tags (goroutine/defer from the golden file).
- testdata/impls: Storer, CompositeIface with embedded interface, same-file localImpl. 6 implements. No mock assertions exist, so no mock tags.
- testdata/writes: cross-package writes to package variables. 5 writes (1 is an absent assertion).
- testdata/data_flow: six trace_data patterns. 8 flows.
- not_translated: 6 entries (four old CLI flags such as -no-dispatches, one log-only block, one out-of-scope note).

Doubts:
- expected.json for dispatch lists ClosureMethodHandler.Serve as a target of the interface closure (class-hierarchy over-approximation); the golden file checks it.
- NoKey via: fixture comment and golden file differ in text; golden wins.
- localImpl: fixture comment says "must NOT emit"; the test says it MUST. The case follows the test.
- samePackageWriter absent case: old helper dropped same-package writes by design; the new adapter may emit them.
- Control-flow cases are marked optional where the old test only logged when the edge was missing.
- min_branch_depth has no tag in the new contract.
