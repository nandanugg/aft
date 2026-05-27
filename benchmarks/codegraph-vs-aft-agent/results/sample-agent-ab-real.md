# AFT vs CodeGraph agent benchmark

- Model: `opencode-go/deepseek-v4-flash-free`
- Fallback model: `opencode-go/deepseek-v4-pro`
- Corpus: `agent-fixture`
- Timestamp: 2026-05-27T05:41:49.528Z
- Tasks: 1
- Dry run: false

## Summary

| arm | runs | successes | success rate | tokens total | median tokens | median wall ms | p95 wall ms | median tool calls |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| aft | 1 | 1 | 1.000 | 13623 | 13623 | 15358 | 15358 | 3 |
| codegraph | 1 | 1 | 1.000 | 10507 | 10507 | 6218 | 6218 | 1 |

## Per task

| task | arm | kind | status | model | wall ms | tokens | tool calls |
| --- | --- | --- | --- | --- | ---: | ---: | ---: |
| answer-loyalty-discount | aft | answer | PASS | opencode-go/deepseek-v4-flash-free | 15358 | 13623 | 3 |
| answer-loyalty-discount | codegraph | answer | PASS | opencode-go/deepseek-v4-flash-free | 6218 | 10507 | 1 |
