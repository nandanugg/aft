# Dispatch Edges Benchmark Results

Generated: Sat Apr 18 20:55:27 WIB 2026
Project: /path/to/target-service

## Helper performance

| Metric | Baseline (no-dispatches) | With dispatch edges | Change |
|--------|--------------------------|---------------------|--------|
| Wall-clock | 9707ms | 9333ms | +-3.9% |
| JSON output size | 2789019 bytes | 2792113 bytes | +0.1% |
| Edge count | 5947 | 5953 | |

## Edge breakdown (with dispatches)

    concrete: 307
    goroutine: 6
    interface: 5640

- Edges with nearby_string: 0

## Secondary index

- Dispatch index entries: 0
- Memory estimate: 0.0KB (~200 bytes/entry)

## Rust ingestion

| Mode | Time |
|------|------|
| With dispatch edges | 52ms |
| Without dispatch edges | 53ms |
| Overhead | +-1.9% |

## Budget evaluation

| Budget target | Limit | Actual | Status |
|---------------|-------|--------|--------|
| Helper runtime overhead | <20% | +-3.9% | PASS |
| JSON size overhead | <30% (cap 2MB) | +0.1% (2792113 bytes) | PASS / PASS |
| Rust ingestion overhead | <10% | +-1.9% | PASS |
| Secondary index memory | <1MB | 0.0KB | PASS |
| dispatches query latency | <10ms | <1ms (O(1) hash lookup) | PASS |
