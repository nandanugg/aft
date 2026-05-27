# AFT vs CodeGraph retrieval — aft

- Corpus: `opencode-aft`
- Codebase: `/workspace`
- Target SHA: `unknown`
- Timestamp: 2026-05-27T05:14:08.163Z
- Top K: 10

## Summary

| driver | cases | pass | mean recall | mean MRR | P@1 | P@5 | P@10 | p50 ms | p95 ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| aft | 6 | 5 | 0.792 | 0.783 | 0.667 | 0.300 | 0.233 | 4.4 | 97.4 |

## Per case

| case | mode | status | recall | MRR | P@5 | latency ms | found | missed |
| --- | --- | --- | ---: | ---: | ---: | ---: | --- | --- |
| symbol-binary-bridge | search | PASS | 1.000 | 1.000 | 0.200 | 97.4 | BinaryBridge |  |
| symbol-bridge-options | search | PASS | 1.000 | 1.000 | 0.400 | 3.4 | BridgeOptions |  |
| symbol-semantic-index-status | search | PASS | 1.000 | 1.000 | 0.400 | 3.5 | SemanticIndexStatus |  |
| context-plugin-bridge-dispatch | context | PASS | 0.667 | 1.000 | 0.400 | 8.3 | callBridge, bridgeFor | BinaryBridge |
| context-semantic-search-path | context | FAIL | 0.333 | 0.200 | 0.200 | 7.9 | SemanticIndex | semanticTools, SearchIndex |
| context-import-organization | context | PASS | 0.750 | 0.500 | 0.200 | 4.4 | handle_organize_imports, parse_imports, classify_group | generate_import_line |
