#!/usr/bin/env bash
# AFT Claude Code Hooks Uninstaller
# Removes AFT hooks from Claude Code

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AFT_ROOT="$(dirname "$SCRIPT_DIR")"
CLAUDE_DIR="$HOME/.claude"
HOOKS_DIR="$CLAUDE_DIR/hooks"
ZSH_CONFIG_FILE="$HOME/.zshrc"
FISH_CONFIG_FILE="${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish"
ENV_BLOCK_START="# >>> aft-go-helper >>>"
ENV_BLOCK_END="# <<< aft-go-helper <<<"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info() { echo -e "${GREEN}[INFO]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }

remove_managed_block() {
  local file="$1"
  local temp_file
  [ -f "$file" ] || return 0
  temp_file="$(mktemp)"
  awk -v start="$ENV_BLOCK_START" -v end="$ENV_BLOCK_END" '
    $0 == start { in_block = 1; next }
    $0 == end { in_block = 0; next }
    !in_block { print }
  ' "$file" > "$temp_file" || return 1
  mv "$temp_file" "$file"
  info "Removed AFT_GO_HELPER_PATH block from $file"
}

# Remove hook files
[ -f "$HOOKS_DIR/aft" ] && rm "$HOOKS_DIR/aft" && info "Removed $HOOKS_DIR/aft"
[ -f "$HOOKS_DIR/aft-hook.sh" ] && rm "$HOOKS_DIR/aft-hook.sh" && info "Removed $HOOKS_DIR/aft-hook.sh"
[ -f "$HOOKS_DIR/aft-session-runtime.sh" ] && rm "$HOOKS_DIR/aft-session-runtime.sh" && info "Removed $HOOKS_DIR/aft-session-runtime.sh"
[ -f "$HOOKS_DIR/aft-session-reminder.sh" ] && rm "$HOOKS_DIR/aft-session-reminder.sh" && info "Removed $HOOKS_DIR/aft-session-reminder.sh"
[ -f "$HOOKS_DIR/aft-session-end.sh" ] && rm "$HOOKS_DIR/aft-session-end.sh" && info "Removed $HOOKS_DIR/aft-session-end.sh"
[ -f "$HOOKS_DIR/aft-code-discovery-gate.sh" ] && rm "$HOOKS_DIR/aft-code-discovery-gate.sh" && info "Removed $HOOKS_DIR/aft-code-discovery-gate.sh"

# Remove AFT.md
[ -f "$CLAUDE_DIR/AFT.md" ] && rm "$CLAUDE_DIR/AFT.md" && info "Removed $CLAUDE_DIR/AFT.md"

# Remove @AFT.md from CLAUDE.md
if [ -f "$CLAUDE_DIR/CLAUDE.md" ]; then
    if grep -q "@AFT.md" "$CLAUDE_DIR/CLAUDE.md"; then
        sed -i '' '/@AFT.md/d' "$CLAUDE_DIR/CLAUDE.md"
        info "Removed @AFT.md from CLAUDE.md"
    fi
fi

# Remove hooks from settings.json
SETTINGS_FILE="$CLAUDE_DIR/settings.json"
if [ -f "$SETTINGS_FILE" ] && command -v jq &>/dev/null; then
    TEMP_FILE=$(mktemp)
    jq '
      def drops_aft:
        (.hooks // []) | map(
          (.command // "") as $c
          | ($c | contains("aft-hook.sh")) or
            ($c | contains("aft-code-discovery-gate.sh")) or
            ($c | contains("aft-session-reminder.sh")) or
            ($c | contains("aft-session-end.sh"))
        ) | any;
      .hooks.PreToolUse = ((.hooks.PreToolUse // []) | map(select(drops_aft | not))) |
      .hooks.SessionStart = ((.hooks.SessionStart // []) | map(select(drops_aft | not))) |
      .hooks.SessionEnd = ((.hooks.SessionEnd // []) | map(select(drops_aft | not)))
    ' "$SETTINGS_FILE" > "$TEMP_FILE" 2>/dev/null && mv "$TEMP_FILE" "$SETTINGS_FILE" && \
        info "Removed AFT hooks from settings.json"
fi

# Remove symlink
[ -L "/usr/local/bin/aft" ] && rm "/usr/local/bin/aft" 2>/dev/null && info "Removed /usr/local/bin/aft symlink"
[ -L "/usr/local/bin/aft-go-helper" ] && [ "$(readlink /usr/local/bin/aft-go-helper)" = "$AFT_ROOT/target/release/aft-go-helper" ] && rm "/usr/local/bin/aft-go-helper" 2>/dev/null && info "Removed /usr/local/bin/aft-go-helper symlink"
remove_managed_block "$ZSH_CONFIG_FILE"
remove_managed_block "$FISH_CONFIG_FILE"

echo ""
echo -e "${GREEN}AFT Claude Code hooks uninstalled.${NC}"
echo "Restart Claude Code to complete removal."
