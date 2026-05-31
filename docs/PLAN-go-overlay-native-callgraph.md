# Go Overlay Native Call Graph Integration Plan

## Goal

Make the native AFT call graph the source of truth for Go semantic relationships.
The Go helper and Go sidecar should provide typed facts; native AFT commands
should own traversal, rendering, filtering, and user-facing semantics.

## Principles

- Keep `CallGraph` as the single relationship store.
- Preserve edge kind. A dispatch registration is not a direct call.
- Treat Go helper output as provider facts, not a separate product surface.
- Hard-migrate native commands to a common typed edge contract.
- Do not preserve fork-specific internal query paths once native graph owns the data.

## Native Contract

Every relationship flowing into call graph traversal is a typed edge:

- `direct_call`
- `concrete_call`
- `interface_call`
- `dispatch_registration`
- `goroutine`
- `defer`
- `implements`
- `writes`

Each edge can carry provider metadata:

- provider: `aft_native`, `go_helper`, or `aft_go_sidecar`
- dispatch key / nearby string
- dispatch receiver / `dispatched_via`
- call context

Native commands should consume this contract directly.

## Move Into Native AFT

- `dispatches` and `dispatched_by` become filters over native typed graph edges.
- `implementations` becomes a native `implements` relationship query.
- `writers` becomes a native `writes` relationship query and later can enrich
  impact/data-flow analysis.
- Go helper metadata becomes native edge metadata:
  `kind`, `nearby_string`, `dispatched_via`, and call context.

## Keep As Infrastructure

- `go-helper` remains the Go semantic analyzer.
- `aft_go_sidecar` remains a cache/runtime provider for helper snapshots.
- `go_overlay_provider` remains a configuration knob.
- `go_overlay_session_open/touch/close` remain lifecycle commands for plugin
  warmup and sidecar leasing.

## Execution Steps

1. Add native typed edge fields to call graph caller sites.
2. Convert Go helper edges into native reverse-index entries.
3. Expose typed inbound edges through native `callers`.
4. Let `trace_to` and `impact` traverse typed edges with labels and metadata.
5. Refactor fork-only query methods onto the native graph.
6. Validate with focused unit tests and settlement_service end-to-end checks.

## Migration Rule

No compatibility side table should remain. Commands such as `dispatches`,
`dispatched_by`, `implementations`, and `writers` may remain as public command
names during the transition, but their implementation must query the native
typed graph contract only.
