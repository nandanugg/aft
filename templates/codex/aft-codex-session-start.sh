#!/usr/bin/env bash
set -euo pipefail

cat <<'EOF'
CRITICAL - AFT Code Discovery Protocol:
1. ALWAYS use AFT tools FIRST for code exploration in an indexed project:
   - aft outline <file|dir>        structure (symbols, functions, types)
   - aft zoom <file> <symbol>      read ONE symbol body with call-graph annotations
   - aft callers <file> <symbol>   who calls this function (reverse graph)
   - aft call_tree <file> <symbol> what this function calls (forward graph)
   - aft trace_to <file> <symbol>  how execution reaches this point
   - aft trace_data <file> <symbol> <expr>  how a value flows across calls
   - aft impact <file> <symbol>    what breaks if this changes
2. Fall back to shell Read/Grep/find ONLY for:
   - plain text/config files (not code)
   - files you are about to edit (many editors need a fresh read first)
   - when AFT tools return zero on a symbol you know exists (dynamic dispatch)
3. First AFT call in a project can take 10-30 seconds (index warm-up + Go helper). This is not a hang.
4. When AFT tools disagree with your prior knowledge or published docs, TRUST THE TOOLS.
   They see the current source; priors can be stale. Report the disagreement explicitly.
5. AFT returns STRUCTURAL facts, not semantic guarantees. Prefer "the helper identifies N handlers"
   over "there are only N handlers". Reserve absolute language for claims you actually checked.
Detailed rules live in `~/.codex/AFT.md`.
EOF
