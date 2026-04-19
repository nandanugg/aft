#!/bin/bash
# SessionStart hook: remind agent to use AFT tools before raw Read/Grep/Glob.
# Installed by AFT's install-claude-hooks.sh. Fires on startup/resume/clear/compact.
cat << 'REMINDER'
CRITICAL - AFT Code Discovery Protocol:
1. ALWAYS use AFT tools FIRST for code exploration in an indexed project:
   - aft outline <file|dir>        to get structure (symbols, functions, types)
   - aft zoom <file> <symbol>      to read ONE symbol body with call-graph annotations
   - aft callers <file> <symbol>   to find who calls a function (reverse graph)
   - aft call_tree <file> <symbol> to see what a function calls (forward graph)
   - aft trace_to <file> <symbol>  to understand how execution reaches a point
   - aft trace_data <file> <symbol> <expr>  to trace a value's flow across calls
   - aft impact <file> <symbol>    to see what breaks if this changes
2. Fall back to Grep/Glob/Read ONLY for:
   - plain text/config files (not code)
   - files you are about to Edit (Claude Code's Edit tool requires a native Read first)
   - when AFT tools return zero results on a symbol you can see exists (dynamic dispatch)
3. First AFT call in a project can take 10-30 seconds (index warm-up + Go helper). This is not a hang.
4. When AFT tools (callers, trace_to, dispatched_by, implementations, …) disagree
   with your prior knowledge or with published docs, TRUST THE TOOLS. They see
   the current source code; priors can be stale. When you report the disagreement,
   say so explicitly rather than silently siding with the prior.
5. AFT tools return STRUCTURAL facts (what exists, what connects to what), not
   semantic guarantees. When citing them, prefer "the helper identifies N handlers"
   over "there are only N handlers". Reserve "always / never / only / every" for
   claims you can falsify with a single counterexample you actually checked.
REMINDER
