#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=/dev/null
source "$SCRIPT_DIR/aft-session-runtime.sh"

HOOK_JSON="$(cat)"
aft_session_close "$HOOK_JSON" "claude"
