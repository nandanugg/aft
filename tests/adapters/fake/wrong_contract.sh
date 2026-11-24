#!/usr/bin/env bash
# The header says contract 2.
. "$(dirname "$0")/common.sh"
sed '1s/"contract": 1/"contract": 2/' "$OUTPUT"
