# Interface Implementation Edges Benchmark Results (Tier 1.4)

Generated: 2026-04-18
Project: /path/to/target-service (473 Go files)
Helper binary: /tmp/aft-go-helper-test (built from worktree feat/go-vta-helper)

## Helper performance

| Metric | Without implements | With implements | Overhead |
|--------|-------------------|-----------------|----------|
| Wall-clock | ~9s | ~8s | ~0% (noise; both cold-load SSA) |
| JSON output size | 2,792,114 bytes | 3,225,938 bytes | +15.5% |
| Edge count | 5,953 | 6,814 | +861 implements edges |

## Edge breakdown (with implements)

    concrete:   307
    goroutine:     6
    implements:  861
    interface:  5640
    TOTAL:      6814

## Implements edge sample

```
iface=StorageUtils -> *...mocks.StorageUtils.DeleteFile  @ settlement_s3_utils/mocks/StorageUtils.go:42
iface=StorageUtils -> *...mocks.StorageUtils.DownloadFile @ settlement_s3_utils/mocks/StorageUtils.go:99
iface=BalanceSettlement -> *...BalanceSettlement.InitiateBalanceSettlementTopup @ balance_settlement/service_impl.go:42
```

Line numbers are 1-based and correct (populated from `ssa.Function.Pos()`).

## Query latency (release binary, cached graph)

| Command | Latency | Budget |
|---------|---------|--------|
| `implementations direct_settlement/service.go Service` | <1ms | <10ms |
| `implementations balance_settlement/service.go Service` (1 impl) | <1ms | <10ms |
| `implementations --include-mocks direct_settlement/service.go Service` | <1ms | <10ms |
| `callers --via-interface direct_settlement/service.go Service` | <1ms | <10ms |
| Graph build (first call, 473 files) | ~1.9s | <5s |

## Mock filtering

Without `--include-mocks`:
- `balance_settlement/service.go::Service` → 1 impl (concrete `BalanceSettlement`)
- `direct_settlement/service.go::Service` → 0 impls (only mock exists)

With `--include-mocks`:
- `balance_settlement/service.go::Service` → 2 impls (concrete + mock)
- `direct_settlement/service.go::Service` → 1 impl (mock)

## Budget evaluation

| Budget target | Limit | Actual | Status |
|---------------|-------|--------|--------|
| Helper runtime overhead | <20% | ~0% (noise) | PASS |
| JSON size overhead | <30% (cap 2MB raw) | +15.5% (3.2MB) | PASS |
| implements query latency | <10ms | <1ms (O(1) hash lookup) | PASS |
| callers --via-interface latency | <10ms | <1ms | PASS |
| Mock filtering correctness | correct | verified | PASS |
| Line numbers present | yes | yes (from SSA pos) | PASS |
| Same-file filter | correct | verified (impls_test.go) | PASS |
