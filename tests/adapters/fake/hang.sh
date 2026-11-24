#!/usr/bin/env bash
# Prints the header, then never ends. Marker for pgrep: "sleep 86397".
. "$(dirname "$0")/common.sh"
head -n 1 "$OUTPUT"
exec sleep 86397
