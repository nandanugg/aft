#!/usr/bin/env bash
# AFT OpenCode uninstaller for the local source-checkout integration.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AFT_ROOT="$(dirname "$SCRIPT_DIR")"
PLUGIN_PACKAGE_DIR="$AFT_ROOT/packages/opencode-plugin"
CONFIG_DIR="${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/opencode}"
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
error() { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

detect_config_path() {
  local base_name="$1"
  if [ -f "$CONFIG_DIR/${base_name}.jsonc" ]; then
    printf '%s' "$CONFIG_DIR/${base_name}.jsonc"
    return
  fi
  if [ -f "$CONFIG_DIR/${base_name}.json" ]; then
    printf '%s' "$CONFIG_DIR/${base_name}.json"
    return
  fi
  printf '%s' "$CONFIG_DIR/${base_name}.json"
}

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

ensure_plugin_deps() {
  if [ -f "$AFT_ROOT/node_modules/comment-json/package.json" ]; then
    return
  fi

  info "Installing Bun dependencies needed to edit OpenCode JSONC config..."
  (
    cd "$AFT_ROOT"
    bun install
  ) || error "Failed to install Bun dependencies"
}

remove_plugin_config() {
  local config_path="$1"
  local label="$2"

  if [ ! -f "$config_path" ]; then
    warn "$config_path does not exist; skipping $label"
    return
  fi

  (
    cd "$PLUGIN_PACKAGE_DIR"
    CONFIG_PATH="$config_path" bun --eval '
      import { readFileSync, writeFileSync } from "node:fs";
      import { parse, stringify } from "comment-json";

      const configPath = process.env.CONFIG_PATH;
      if (!configPath) {
        throw new Error("Missing config path");
      }

      const matchesAftEntry = (entry) =>
        entry.includes("@cortexkit/aft-opencode") ||
        entry.includes("/packages/opencode-plugin/") ||
        entry.includes("\\packages\\opencode-plugin\\") ||
        entry.includes("/aft-opencode") ||
        entry.includes("\\aft-opencode");

      const parsed = parse(readFileSync(configPath, "utf-8"));
      const config =
        parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : {};

      const existingPlugins = Array.isArray(config.plugin)
        ? config.plugin.filter((entry) => typeof entry === "string")
        : [];
      const nextPlugins = existingPlugins.filter((entry) => !matchesAftEntry(entry));

      if (nextPlugins.length > 0) {
        config.plugin = nextPlugins;
      } else {
        delete config.plugin;
      }

      writeFileSync(configPath, `${stringify(config, null, 2)}\n`);
    '
  ) || error "Failed to update $config_path"

  info "Removed $label from $config_path"
}

remove_aft_config() {
  local config_path="$1"
  local label="$2"

  if [ ! -f "$config_path" ]; then
    warn "$config_path does not exist; skipping $label"
    return
  fi

  (
    cd "$PLUGIN_PACKAGE_DIR"
    CONFIG_PATH="$config_path" bun --eval '
      import { readFileSync, writeFileSync } from "node:fs";
      import { parse, stringify } from "comment-json";

      const configPath = process.env.CONFIG_PATH;
      if (!configPath) {
        throw new Error("Missing config path");
      }

      const parsed = parse(readFileSync(configPath, "utf-8"));
      const config =
        parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : {};

      if (config.go_overlay_provider === "aft_go_sidecar" || config.go_overlay_provider === "sidecar") {
        delete config.go_overlay_provider;
      }

      writeFileSync(configPath, `${stringify(config, null, 2)}\n`);
    '
  ) || error "Failed to update $config_path"

  info "Removed $label from $config_path"
}

command -v bun >/dev/null 2>&1 || error "bun is required but not installed or not on PATH."

ensure_plugin_deps

remove_plugin_config "$(detect_config_path opencode)" "OpenCode server plugin entry"
remove_aft_config "$(detect_config_path aft)" "OpenCode AFT Go overlay setting"
remove_plugin_config "$(detect_config_path tui)" "OpenCode TUI plugin entry"
remove_managed_block "$ZSH_CONFIG_FILE"
remove_managed_block "$FISH_CONFIG_FILE"

echo ""
echo -e "${GREEN}AFT OpenCode integration uninstalled.${NC}"
echo "Restart OpenCode to complete removal."
