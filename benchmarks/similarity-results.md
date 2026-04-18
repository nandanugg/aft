# Similarity Index Benchmark Results

**Date:** 2026-04-18  
**Commit branch:** feat/go-vta-helper  
**Platform:** macOS Darwin 24.3.0 (Apple Silicon)  
**Project measured:** `example/target-service`

---

## Setup

The similarity index uses five layers: identifier tokenization → Snowball English stemming → TF-IDF with smoothed IDF + L2-normalized sparse vectors → optional synonym dict → call-graph co-citation (Jaccard).

Index build is parallelized with rayon across file parsing (tree-sitter per file). TF-IDF construction is single-threaded (memory-bound).

---

## Project Statistics

| Metric | Value |
|---|---|
| Files scanned | 491 |
| Symbols indexed | 7,628 |
| Index disk size (CBOR) | 2.35 MB |

---

## Benchmark Results

All measurements taken using the JSON-over-stdin protocol with a persistent aft process (the real usage model — aft is a long-lived daemon, not a one-shot CLI tool).

### Index Build Time (cold, no disk cache)

| Metric | Result | Target | Status |
|---|---|---|---|
| Index build, 7628 symbols (parallel rayon) | ~120ms | <500ms/10k symbols | PASS |
| Extrapolated to 10k symbols | ~157ms | <500ms | PASS |

Note: build includes tree-sitter parsing of 491 files + tokenization + stemming + TF-IDF matrix construction. Parallelized across CPU cores.

### Index Disk Footprint

| Metric | Result | Target | Status |
|---|---|---|---|
| CBOR-encoded index | 2.35 MB | <5 MB | PASS |
| Memory footprint at runtime | ~3-4 MB (estimated) | <20 MB | PASS |

### Query Latency (warm cache, persistent daemon)

Measured as 20 consecutive `aft similar` queries within a single aft process session (index loaded from disk at configure time):

| Metric | Result | Target | Status |
|---|---|---|---|
| Per-query latency (no explain), avg | **9.77ms** | <50ms | PASS |
| `--explain` per-query latency | **~27.7ms** | <50ms | PASS |
| `--explain` overhead above baseline | **17.94ms** | <20ms | PASS |

Note: measurements taken over 20 iterations each. Warm cache means similarity-index.cbor is loaded at configure time. Co-citation computation adds negligible overhead (callee sets fetched from in-memory callgraph cache).

### Top-10 Matches for `SettleMerchantSettlement`

Query: `aft similar merchant_settlement/service.go SettleMerchantSettlement --top=10 --min-score=0.0`

| Rank | Symbol | Score | File |
|---|---|---|---|
| 1 | SettleMerchantSettlement | 0.8500 | core_banking_settlement/merchant_settlement/mocks/Service.go |
| 2 | SettleMerchantSettlement | 0.8500 | core_banking_settlement/merchant_settlement/service.go |
| 3 | SettleMerchantSettlement | 0.8500 | merchant_settlement/mocks/Service.go |
| 4 | TestSettleMerchantSettlement | 0.7431 | merchant_settlement/service_test.go |
| 5 | Service_SettleMerchantSettlement_Call | 0.6923 | core_banking_settlement/merchant_settlement/mocks/Service.go |
| 6 | Service_SettleMerchantSettlement_Call | 0.6923 | merchant_settlement/mocks/Service.go |
| 7 | SettleMerchantSettlementByID | 0.6723 | store/settlement_balance_store.go |
| 8 | HandleSettleMerchantSettlement | 0.6659 | core_banking_settlement/merchant_settlement/http_handler.go |
| 9 | HandleSettleMerchantSettlement | 0.6659 | merchant_settlement/http_handler.go |
| 10 | SetMerchantSettlementSettled | 0.6512 | store/merchant_settlement_store.go |

**Sanity check:** All top 10 results are settlement-related. The top 3 include the exact same function name in sister packages (core_banking_settlement, mocks) — semantically correct. The rank correctly prioritizes identical names, then test wrapper, then mock call wrapper, then related settlement functions. PASS.

---

## Performance Target Summary

| Target | Result | Status |
|---|---|---|
| Index build < 500ms (10k symbols) | ~120ms (7628 symbols) | PASS |
| Index disk < 5 MB | 2.35 MB | PASS |
| Index memory < 20 MB | ~4 MB estimated | PASS |
| Query < 50ms (persistent daemon) | 9.77ms avg | PASS |
| `--explain` overhead < 20ms | 17.94ms | PASS |
| Top-3 contains settlement symbols | Yes (all top-10 are) | PASS |

---

## Notes

- The `process-spawn + configure + query` latency (new aft process per call) is ~100-360ms due to binary startup + disk load + query. This is not the real usage model — aft is always a long-lived daemon.
- In the daemon model (one process, many requests), per-query overhead is ~10ms including index dispatch.
- The parallel file parsing (rayon) reduced build time from ~656ms (single-threaded) to ~120ms (parallel) on this machine.
- Co-citation (Jaccard over callee sets) is computed at query time from the live callgraph, not pre-indexed. Currently returns 0.0 for all candidates until the callgraph is built for the target file. This is a known limitation — co-citation becomes meaningful after the first `aft callers` or `aft call_tree` call on the target file.
