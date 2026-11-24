#!/usr/bin/env bash
# A fact of kind "writes", which the test registry entry does not declare.
. "$(dirname "$0")/common.sh"
sed 's/"kind": "implements"/"kind": "writes"/' "$OUTPUT"
