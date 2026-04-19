#!/bin/bash
# Gate hook: nudges Claude toward AFT tools on first code-discovery call.
# First Grep/Glob/Read/Search per session -> block once with guidance.
# Subsequent calls -> allow (native or aft-hook.sh handling).
# PPID = Claude Code process PID, unique per session.
GATE=/tmp/aft-code-discovery-gate-$PPID
find /tmp -name 'aft-code-discovery-gate-*' -mtime +1 -delete 2>/dev/null
if [ -f "$GATE" ]; then
    exit 0
fi
touch "$GATE"
echo 'BLOCKED: For code discovery, use AFT semantic tools first: `aft outline <file|dir>` for structure, `aft trace_to`/`aft call_tree`/`aft callers` for behavior questions, `aft zoom <file> <symbol>` to read one symbol, `aft trace_data` for value flow. Fall back to Grep/Glob/Read only for text/config files, or when you are about to Edit a file (native Read required). Retry with the appropriate AFT command, or retry this call if the answer genuinely needs raw search.' >&2
exit 2
