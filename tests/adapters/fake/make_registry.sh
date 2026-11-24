#!/usr/bin/env bash
# Prints a test registry with absolute paths.
#   make_registry.sh <go-fake> [<javascript-fake>]   (file names in this directory, e.g. ok.sh hang.sh)
# Optional env: FAKE_TIMEOUT_S (default 5), FAKE_WAIT_S (default 2), FAKE_COOLDOWN_S (default 600).
# Each entry sets env AFT_FAKE_ENTRY=<language>. The go entry declares the fact kind "implements" and hides tag "mock"; markers: go.mod / package.json.
set -eu
dir=$(cd "$(dirname "$0")" && pwd)
go_fake=$1
js_fake=${2:-}
timeout=${FAKE_TIMEOUT_S:-5}
wait=${FAKE_WAIT_S:-2}
cooldown=${FAKE_COOLDOWN_S:-600}
printf '{"adapters": [\n'
printf '  {"contract": 1, "language": "go", "root_markers": ["go.mod"],\n'
printf '   "command": ["%s/%s", "--language", "go", "--root", "{root}"],\n' "$dir" "$go_fake"
printf '   "env": {"AFT_FAKE_ENTRY": "go"}, "timeout_s": %s, "query_wait_s": %s, "failure_cooldown_s": %s, "hidden_tags": ["mock"],\n' "$timeout" "$wait" "$cooldown"
printf '   "facts": [{"kind": "implements", "description": "type -> interface", "aliases": [{"name": "implementations", "by": "object"}]}]}'
if [ -n "$js_fake" ]; then
	printf ',\n  {"contract": 1, "language": "javascript", "root_markers": ["package.json"],\n'
	printf '   "command": ["%s/%s", "--language", "javascript", "--root", "{root}"],\n' "$dir" "$js_fake"
	printf '   "env": {"AFT_FAKE_ENTRY": "javascript"}, "timeout_s": %s, "query_wait_s": %s, "failure_cooldown_s": %s}' "$timeout" "$wait" "$cooldown"
fi
printf '\n]}\n'
