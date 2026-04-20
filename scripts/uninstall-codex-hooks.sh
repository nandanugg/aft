#!/usr/bin/env bash
# AFT Codex Hooks Uninstaller
# Removes AFT hook integration from Codex.

set -euo pipefail

CODEX_DIR="$HOME/.codex"
CODEX_HOOKS_DIR="$CODEX_DIR/hooks"
CODEX_BIN_DIR="$CODEX_DIR/bin"
CODEX_AGENTS_FILE="$CODEX_DIR/AGENTS.md"
CODEX_HOOKS_FILE="$CODEX_DIR/hooks.json"
CODEX_CONFIG_FILE="$CODEX_DIR/config.toml"
CODEX_AFT_DOC="$CODEX_DIR/AFT.md"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info() { echo -e "${GREEN}[INFO]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }

remove_if_exists() {
  local path="$1"
  if [ -e "$path" ]; then
    rm -f "$path"
    info "Removed $path"
  fi
}

remove_if_exists "$CODEX_BIN_DIR/aft"
remove_if_exists "$CODEX_HOOKS_DIR/aft-codex-session-start.sh"
remove_if_exists "$CODEX_HOOKS_DIR/aft-codex-user-prompt-submit.sh"
remove_if_exists "$CODEX_AFT_DOC"

if [ -f "$CODEX_AGENTS_FILE" ]; then
  TEMP_FILE="$(mktemp)"
  awk '!/AFT\.md/' "$CODEX_AGENTS_FILE" > "$TEMP_FILE"
  mv "$TEMP_FILE" "$CODEX_AGENTS_FILE"
  info "Removed AFT.md reference from AGENTS.md"
fi

if [ -f "$CODEX_HOOKS_FILE" ] && command -v jq >/dev/null 2>&1; then
  TEMP_FILE="$(mktemp)"
  jq '
    .hooks = (.hooks // {}) |
    .hooks.SessionStart = (
      (.hooks.SessionStart // []) | map(
        . as $entry |
        (($entry.hooks // []) | map(select((.command // "") | contains("aft-codex-session-start.sh"))) | length) as $aft |
        if $aft > 0 then empty else $entry end
      )
    ) |
    .hooks.UserPromptSubmit = (
      (.hooks.UserPromptSubmit // []) | map(
        . as $entry |
        (($entry.hooks // []) | map(select((.command // "") | contains("aft-codex-user-prompt-submit.sh"))) | length) as $aft |
        if $aft > 0 then empty else $entry end
      )
    )
  ' "$CODEX_HOOKS_FILE" > "$TEMP_FILE" && mv "$TEMP_FILE" "$CODEX_HOOKS_FILE" && \
    info "Removed AFT hooks from hooks.json"
fi

if [ -L "/usr/local/bin/aft" ] && [ "$(readlink /usr/local/bin/aft)" = "$CODEX_BIN_DIR/aft" ]; then
  rm -f /usr/local/bin/aft
  info "Removed /usr/local/bin/aft symlink"
fi

echo ""
echo -e "${GREEN}AFT Codex integration uninstalled.${NC}"
echo "The codex_hooks feature and unstable-feature warning suppression were left in $CODEX_CONFIG_FILE."
echo "Restart Codex to complete removal."
