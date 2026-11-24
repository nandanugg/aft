#!/usr/bin/env bash
# A valid Go run with no sites and no analysed files: the overlay adds nothing (R2's first gate, decision K).
. "$(dirname "$0")/common.sh"
echo '{"type": "header", "contract": 1, "language": "go", "tool": "fake-empty", "tool_version": "0"}'
echo '{"type": "analysed", "files": []}'
echo '{"type": "end", "sites": 0, "targets": 0, "facts": 0, "analysed_files": 0, "unanalysed_files": 0}'
