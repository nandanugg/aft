#!/usr/bin/env bash
# Prints the header and one site, writes to stderr, then dies on SIGABRT.
. "$(dirname "$0")/common.sh"
head -n 3 "$OUTPUT"
echo "fake adapter: about to crash" >&2
kill -ABRT $$
