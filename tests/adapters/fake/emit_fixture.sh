#!/usr/bin/env bash
# Prints the fixture overlay of the root from the private case pack:
# $AFT_ACCEPT_CASES/fixtures/overlays/<root basename>/go.ndjson.gz (the pack is not part of this repository).
. "$(dirname "$0")/common.sh"
[ -n "${AFT_ACCEPT_CASES:-}" ] || { echo "emit_fixture.sh needs AFT_ACCEPT_CASES (the private case pack)" >&2; exit 2; }
gunzip -c "$AFT_ACCEPT_CASES/fixtures/overlays/$(basename "$ROOT")/go.ndjson.gz"
