#!/usr/bin/env bash
# A valid but PARTIAL run (Go only): a.go analysed, b.go unanalysed; status must be partial, never fresh.
. "$(dirname "$0")/common.sh"
head -n 1 "$OUTPUT"
echo '{"type": "analysed", "files": ["a.go"]}'
grep '"type": "site", "file": "a.go"' "$OUTPUT"
grep '"type": "fact"' "$OUTPUT"
echo '{"type": "unanalysed", "files": ["b.go"], "reason": "package did not type-check: missing module example.com/missing", "unit": "example.com/tiny/b"}'
echo '{"type": "end", "sites": 1, "targets": 1, "facts": 1, "analysed_files": 1, "unanalysed_files": 1}'
