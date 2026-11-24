#!/usr/bin/env bash
# Writes 30 numbered stderr lines and exits 3 (the failure record keeps the last 20: 11..30).
. "$(dirname "$0")/common.sh"
head -n 1 "$OUTPUT"
for i in $(seq 1 30); do echo "fake stderr line $i" >&2; done
exit 3
