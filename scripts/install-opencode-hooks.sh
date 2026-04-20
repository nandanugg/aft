#!/usr/bin/env bash
# AFT OpenCode installer for an unpublished local checkout.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AFT_ROOT="$(dirname "$SCRIPT_DIR")"
PLUGIN_PACKAGE_DIR="$AFT_ROOT/packages/opencode-plugin"
CONFIG_DIR="${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/opencode}"

SERVER_PLUGIN_PATH="$AFT_ROOT/packages/opencode-plugin/src/index.ts"
TUI_PLUGIN_PATH="$AFT_ROOT/packages/opencode-plugin/src/tui/index.tsx"
AFT_BINARY="$AFT_ROOT/target/release/aft"

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

ensure_plugin_deps() {
  if [ -f "$AFT_ROOT/node_modules/@opencode-ai/plugin/package.json" ] \
    && [ -f "$AFT_ROOT/node_modules/comment-json/package.json" ]; then
    return
  fi

  info "Installing Bun dependencies for the local OpenCode plugin..."
  (
    cd "$AFT_ROOT"
    bun install
  ) || error "Failed to install Bun dependencies"
}

update_plugin_config() {
  local config_path="$1"
  local schema_url="$2"
  local plugin_entry="$3"
  local label="$4"

  (
    cd "$PLUGIN_PACKAGE_DIR"
    CONFIG_PATH="$config_path" CONFIG_SCHEMA="$schema_url" PLUGIN_ENTRY="$plugin_entry" bun --eval '
      import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
      import { dirname } from "node:path";
      import { parse, stringify } from "comment-json";

      const configPath = process.env.CONFIG_PATH;
      const schemaUrl = process.env.CONFIG_SCHEMA;
      const pluginEntry = process.env.PLUGIN_ENTRY;

      if (!configPath || !schemaUrl || !pluginEntry) {
        throw new Error("Missing config update parameters");
      }

      const matchesAftEntry = (entry) =>
        entry.includes("@cortexkit/aft-opencode") ||
        entry.includes("/packages/opencode-plugin/") ||
        entry.includes("\\packages\\opencode-plugin\\") ||
        entry.includes("/aft-opencode") ||
        entry.includes("\\aft-opencode");

      let config = {};
      if (existsSync(configPath)) {
        const parsed = parse(readFileSync(configPath, "utf-8"));
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
          config = parsed;
        }
      }

      const existingPlugins = Array.isArray(config.plugin)
        ? config.plugin.filter((entry) => typeof entry === "string")
        : [];
      const nextPlugins = existingPlugins.filter((entry) => !matchesAftEntry(entry));
      nextPlugins.push(pluginEntry);

      config.plugin = nextPlugins;
      if (typeof config.$schema !== "string" || config.$schema.length === 0) {
        config.$schema = schemaUrl;
      }

      mkdirSync(dirname(configPath), { recursive: true });
      writeFileSync(configPath, `${stringify(config, null, 2)}\n`);
    '
  ) || error "Failed to update $config_path"

  info "Configured $label in $config_path"
}

command -v bun >/dev/null 2>&1 || error "bun is required but not installed or not on PATH."
command -v cargo >/dev/null 2>&1 || error "cargo is required but not installed or not on PATH."
command -v opencode >/dev/null 2>&1 || error "opencode is required but not installed or not on PATH."

[ -f "$SERVER_PLUGIN_PATH" ] || error "Server plugin entry not found at $SERVER_PLUGIN_PATH"
[ -f "$TUI_PLUGIN_PATH" ] || error "TUI plugin entry not found at $TUI_PLUGIN_PATH"

ensure_plugin_deps

info "Building local AFT binary..."
(
  cd "$AFT_ROOT"
  cargo build --release
) || error "Failed to build AFT binary"

[ -x "$AFT_BINARY" ] || error "Expected built binary at $AFT_BINARY"
info "AFT binary: $AFT_BINARY"

OPENCODE_CONFIG_PATH="$(detect_config_path opencode)"
TUI_CONFIG_PATH="$(detect_config_path tui)"

update_plugin_config "$OPENCODE_CONFIG_PATH" "https://opencode.ai/config.json" "$SERVER_PLUGIN_PATH" "OpenCode server plugin"
update_plugin_config "$TUI_CONFIG_PATH" "https://opencode.ai/tui.json" "$TUI_PLUGIN_PATH" "OpenCode TUI plugin"

echo ""
echo -e "${GREEN}AFT OpenCode integration installed successfully!${NC}"
echo ""
echo "Configured files:"
echo "  $OPENCODE_CONFIG_PATH"
echo "  $TUI_CONFIG_PATH"
echo ""
echo "Notes:"
echo "  OpenCode now loads this checkout's plugin source directly."
echo "  The plugin prefers $AFT_BINARY over cached or published binaries."
echo "  If you move this repo, rerun this installer to refresh the absolute plugin paths."
echo ""
echo "Restart OpenCode to activate the local plugin."
