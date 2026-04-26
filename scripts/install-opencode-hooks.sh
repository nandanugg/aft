#!/usr/bin/env bash
# AFT OpenCode installer for an unpublished local checkout.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AFT_ROOT="$(dirname "$SCRIPT_DIR")"
PLUGIN_PACKAGE_DIR="$AFT_ROOT/packages/opencode-plugin"
CONFIG_DIR="${OPENCODE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/opencode}"
ZSH_CONFIG_FILE="$HOME/.zshrc"
FISH_CONFIG_FILE="${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish"
ENV_BLOCK_START="# >>> aft-go-helper >>>"
ENV_BLOCK_END="# <<< aft-go-helper <<<"

SERVER_PLUGIN_PATH="$AFT_ROOT/packages/opencode-plugin/src/index.ts"
TUI_PLUGIN_PATH="$AFT_ROOT/packages/opencode-plugin/src/tui/index.tsx"
AFT_BINARY="$AFT_ROOT/target/release/aft"
GO_HELPER_BINARY="$AFT_ROOT/target/release/aft-go-helper"

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

strip_managed_block() {
  local file="$1"
  local temp_file
  temp_file="$(mktemp)"
  if [ -f "$file" ]; then
    awk -v start="$ENV_BLOCK_START" -v end="$ENV_BLOCK_END" '
      $0 == start { in_block = 1; next }
      $0 == end { in_block = 0; next }
      !in_block { print }
    ' "$file" > "$temp_file" || error "Failed to update $file"
  fi
  printf '%s' "$temp_file"
}

upsert_shell_helper_env() {
  local helper_path="$1"
  local file="$2"
  local shell_kind="$3"
  local temp_file
  temp_file="$(strip_managed_block "$file")"
  mkdir -p "$(dirname "$file")"
  if [ -s "$temp_file" ]; then
    printf '\n' >> "$temp_file"
  fi
  {
    printf '%s\n' "$ENV_BLOCK_START"
    if [ "$shell_kind" = "fish" ]; then
      printf 'set -gx AFT_GO_HELPER_PATH "%s"\n' "$helper_path"
    else
      printf 'export AFT_GO_HELPER_PATH="%s"\n' "$helper_path"
    fi
    printf '%s\n' "$ENV_BLOCK_END"
  } >> "$temp_file"
  mv "$temp_file" "$file"
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

update_aft_config() {
  local config_path="$1"
  local label="$2"

  (
    cd "$PLUGIN_PACKAGE_DIR"
    CONFIG_PATH="$config_path" bun --eval '
      import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
      import { dirname } from "node:path";
      import { parse, stringify } from "comment-json";

      const configPath = process.env.CONFIG_PATH;

      if (!configPath) {
        throw new Error("Missing AFT config update parameters");
      }

      let config = {};
      if (existsSync(configPath)) {
        const parsed = parse(readFileSync(configPath, "utf-8"));
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
          config = parsed;
        }
      }

      config.go_overlay_provider = "aft_go_sidecar";

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

if command -v go >/dev/null 2>&1; then
  info "Building aft-go-helper (Go interface-dispatch resolver)..."
  (
    cd "$AFT_ROOT/go-helper"
    go build -o "$GO_HELPER_BINARY" .
  ) || error "Failed to build aft-go-helper"
elif [ -x "$GO_HELPER_BINARY" ]; then
  info "Using existing aft-go-helper: $GO_HELPER_BINARY"
else
  warn "Go toolchain not found — skipping aft-go-helper build. Install Go for type-accurate call resolution in Go projects."
  GO_HELPER_BINARY=""
fi

info "Building local AFT binary..."
(
  cd "$AFT_ROOT"
  cargo build --release
) || error "Failed to build AFT binary"

[ -x "$AFT_BINARY" ] || error "Expected built binary at $AFT_BINARY"
info "AFT binary: $AFT_BINARY"
if [ -n "$GO_HELPER_BINARY" ] && [ -x "$GO_HELPER_BINARY" ]; then
  upsert_shell_helper_env "$GO_HELPER_BINARY" "$ZSH_CONFIG_FILE" "zsh"
  upsert_shell_helper_env "$GO_HELPER_BINARY" "$FISH_CONFIG_FILE" "fish"
  info "Configured AFT_GO_HELPER_PATH in $ZSH_CONFIG_FILE and $FISH_CONFIG_FILE"
fi

OPENCODE_CONFIG_PATH="$(detect_config_path opencode)"
OPENCODE_AFT_CONFIG_PATH="$(detect_config_path aft)"
TUI_CONFIG_PATH="$(detect_config_path tui)"

update_plugin_config "$OPENCODE_CONFIG_PATH" "https://opencode.ai/config.json" "$SERVER_PLUGIN_PATH" "OpenCode server plugin"
update_aft_config "$OPENCODE_AFT_CONFIG_PATH" "OpenCode AFT config"
update_plugin_config "$TUI_CONFIG_PATH" "https://opencode.ai/tui.json" "$TUI_PLUGIN_PATH" "OpenCode TUI plugin"

echo ""
echo -e "${GREEN}AFT OpenCode integration installed successfully!${NC}"
echo ""
echo "Configured files:"
echo "  $OPENCODE_CONFIG_PATH"
echo "  $OPENCODE_AFT_CONFIG_PATH"
echo "  $TUI_CONFIG_PATH"
if [ -n "$GO_HELPER_BINARY" ] && [ -x "$GO_HELPER_BINARY" ]; then
  echo "  $ZSH_CONFIG_FILE / $FISH_CONFIG_FILE      - AFT_GO_HELPER_PATH"
fi
echo ""
echo "Notes:"
echo "  OpenCode now loads this checkout's plugin source directly."
echo "  The plugin prefers $AFT_BINARY over cached or published binaries."
echo "  OpenCode AFT config now defaults Go overlay queries to the warm AFT-Go sidecar."
echo "  If you move this repo, rerun this installer to refresh the absolute plugin paths."
echo ""
echo "Restart OpenCode to activate the local plugin."
