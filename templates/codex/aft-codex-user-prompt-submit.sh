#!/usr/bin/env bash
set -euo pipefail

command -v jq >/dev/null 2>&1 || exit 0

INPUT="$(cat)"
PROMPT="$(printf '%s' "$INPUT" | jq -r '.prompt // ""')"
[ -z "$PROMPT" ] && exit 0

PROMPT_LOWER="$(printf '%s' "$PROMPT" | tr '[:upper:]' '[:lower:]')"

matches() {
  printf '%s' "$PROMPT_LOWER" | grep -Eq "$1"
}

if matches 'read all|all files|whole repo|entire repo|entire directory|whole directory|all contents|read the repo'; then
  cat <<'EOF'
AFT reminder: for directories or many files, start with `aft outline <dir>` and then narrow to the few files or symbols that actually matter.
EOF
elif matches 'how does|happy path|control flow|execution reach|trace|call tree|call graph|who calls|what calls|callers|impact|what breaks|flow'; then
  cat <<'EOF'
AFT reminder: this is a behavior question. Reach for `aft trace_to`, `aft call_tree`, `aft callers`, `aft impact`, or `aft trace_data` before reconstructing the flow from full-file reads.
EOF
elif matches 'what files|what exists|symbols|file structure|directory structure|list files|outline'; then
  cat <<'EOF'
AFT reminder: this is a structure question. Use `aft outline <file|dir>` first.
EOF
elif matches 'inspect|show me|open file|contents of|look at|read '; then
  cat <<'EOF'
AFT reminder: if you only need targeted inspection, prefer `aft read <file> [start] [limit]`, and use `aft zoom <file> <symbol>` when the question is about one function or type.
EOF
fi
