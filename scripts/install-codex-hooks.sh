#!/usr/bin/env bash
# AFT Codex Hooks Installer
# Installs AFT guidance hooks and CLI wrapper for Codex.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AFT_ROOT="$(dirname "$SCRIPT_DIR")"
CODEX_DIR="$HOME/.codex"
CODEX_HOOKS_DIR="$CODEX_DIR/hooks"
CODEX_BIN_DIR="$CODEX_DIR/bin"
CODEX_AGENTS_FILE="$CODEX_DIR/AGENTS.md"
CODEX_HOOKS_FILE="$CODEX_DIR/hooks.json"
CODEX_CONFIG_FILE="$CODEX_DIR/config.toml"
CODEX_AFT_DOC="$CODEX_DIR/AFT.md"
ZSH_CONFIG_FILE="$HOME/.zshrc"
FISH_CONFIG_FILE="${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish"
ENV_BLOCK_START="# >>> aft-go-helper >>>"
ENV_BLOCK_END="# <<< aft-go-helper <<<"

WRAPPER_TEMPLATE="$AFT_ROOT/templates/aft-wrapper.sh"
SESSION_RUNTIME_TEMPLATE="$AFT_ROOT/templates/aft-session-runtime.sh"
CODEX_AFT_TEMPLATE="$AFT_ROOT/templates/codex/AFT.md"
SESSION_START_TEMPLATE="$AFT_ROOT/templates/codex/aft-codex-session-start.sh"
STOP_TEMPLATE="$AFT_ROOT/templates/codex/aft-codex-stop.sh"
USER_PROMPT_TEMPLATE="$AFT_ROOT/templates/codex/aft-codex-user-prompt-submit.sh"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

info() { echo -e "${GREEN}[INFO]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

escape_sed_replacement() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//&/\\&}"
  printf '%s' "$value"
}

overwrite_file() {
  local source="$1"
  local target="$2"
  if [ -e "$target" ] && cmp -s "$source" "$target"; then
    rm -f "$source"
    return 0
  fi
  cat "$source" > "$target"
  rm -f "$source"
}

copy_if_changed() {
  local source="$1"
  local target="$2"
  local temp_file
  temp_file="$(mktemp)"
  cat "$source" > "$temp_file"
  overwrite_file "$temp_file" "$target"
}

install_templated_file() {
  local source="$1"
  local target="$2"
  local replacement="$3"
  local escaped
  local temp_file
  escaped="$(escape_sed_replacement "$replacement")"
  temp_file="$(mktemp)"
  sed "s|__AFT_BINARY_PATH__|$escaped|g" "$source" > "$temp_file"
  overwrite_file "$temp_file" "$target"
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
  overwrite_file "$temp_file" "$file"
}

can_query_toml_with_yq() {
  command -v yq >/dev/null 2>&1 || return 1
  [ -s "$CODEX_CONFIG_FILE" ] || return 1
  yq -p toml -o json '.' "$CODEX_CONFIG_FILE" >/dev/null 2>&1
}

config_already_has_codex_settings() {
  can_query_toml_with_yq || return 1
  [ "$(yq -p toml -r '(.suppress_unstable_features_warning == true) and (.features.codex_hooks == true)' "$CODEX_CONFIG_FILE" 2>/dev/null)" = "true" ]
}

ensure_root_boolean() {
  local key="$1"
  local value="$2"
  local temp_file
  temp_file="$(mktemp)"

  awk -v key="$key" -v value="$value" '
    BEGIN {
      inserted = 0
      table_seen = 0
    }
    {
      if (!table_seen && $0 ~ "^[[:space:]]*" key "[[:space:]]*=") {
        print key " = " value
        inserted = 1
        next
      }

      if ($0 ~ /^[[:space:]]*\[[^]]+\][[:space:]]*$/) {
        if (!inserted) {
          print key " = " value
          print ""
          inserted = 1
        }
        table_seen = 1
      }

      print
    }
    END {
      if (!inserted) {
        if (NR > 0) {
          print ""
        }
        print key " = " value
      }
    }
  ' "$CODEX_CONFIG_FILE" > "$temp_file" || error "Failed to update $CODEX_CONFIG_FILE"

  overwrite_file "$temp_file" "$CODEX_CONFIG_FILE"
}

ensure_features_boolean() {
  local key="$1"
  local value="$2"
  local temp_file
  temp_file="$(mktemp)"

  awk -v key="$key" -v value="$value" '
    BEGIN {
      in_features = 0
      features_seen = 0
      key_written = 0
    }
    /^[[:space:]]*\[[^]]+\][[:space:]]*$/ {
      if (in_features && !key_written) {
        print key " = " value
        key_written = 1
      }

      if ($0 ~ /^[[:space:]]*\[features\][[:space:]]*$/) {
        in_features = 1
        features_seen = 1
      } else {
        in_features = 0
      }

      print
      next
    }
    {
      if (in_features && $0 ~ "^[[:space:]]*" key "[[:space:]]*=") {
        print key " = " value
        key_written = 1
        next
      }

      print
    }
    END {
      if (!features_seen) {
        print ""
        print "[features]"
        print key " = " value
      } else if (in_features && !key_written) {
        print key " = " value
      }
    }
  ' "$CODEX_CONFIG_FILE" > "$temp_file" || error "Failed to update $CODEX_CONFIG_FILE"

  overwrite_file "$temp_file" "$CODEX_CONFIG_FILE"
}

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    error "Codex hooks are currently not supported on Windows."
    ;;
esac

command -v jq >/dev/null 2>&1 || error "jq is required but not installed. Install with: brew install jq"
command -v codex >/dev/null 2>&1 || error "codex is required but not installed or not on PATH."

# Build AFT binary if needed.
AFT_BINARY="$AFT_ROOT/target/release/aft"
if [ ! -x "$AFT_BINARY" ]; then
  info "Building AFT binary..."
  (
    cd "$AFT_ROOT"
    cargo build --release
  ) || error "Failed to build AFT binary"
fi
info "AFT binary: $AFT_BINARY"

# Build the optional Go helper for interface-dispatch resolution in Go projects.
GO_HELPER_BINARY="$AFT_ROOT/target/release/aft-go-helper"
if command -v go >/dev/null 2>&1; then
  info "Building aft-go-helper (Go interface-dispatch resolver)..."
  if (
    cd "$AFT_ROOT/go-helper"
    go build -o "$GO_HELPER_BINARY" .
  ); then
    info "Go helper built: $GO_HELPER_BINARY"
  else
    warn "Failed to build aft-go-helper — Go interface dispatch resolution will be unavailable."
    GO_HELPER_BINARY=""
  fi
else
  if [ -x "$GO_HELPER_BINARY" ]; then
    info "Using existing aft-go-helper: $GO_HELPER_BINARY"
  else
    warn "Go toolchain not found — skipping aft-go-helper build. Install Go for type-accurate call resolution in Go projects."
    GO_HELPER_BINARY=""
  fi
fi

mkdir -p "$CODEX_HOOKS_DIR" "$CODEX_BIN_DIR"
info "Prepared Codex directories under $CODEX_DIR"

if [ ! -e "$CODEX_CONFIG_FILE" ]; then
  : > "$CODEX_CONFIG_FILE"
fi

# Install the aft wrapper.
escaped_binary="$(escape_sed_replacement "$AFT_BINARY")"
WRAPPER_TEMP_FILE="$(mktemp)"
sed "s|__AFT_BINARY_PATH__|$escaped_binary|g" "$WRAPPER_TEMPLATE" > "$WRAPPER_TEMP_FILE"
overwrite_file "$WRAPPER_TEMP_FILE" "$CODEX_BIN_DIR/aft"
chmod +x "$CODEX_BIN_DIR/aft"
info "Installed CLI wrapper: $CODEX_BIN_DIR/aft"

# Install Codex-specific instructions and hooks.
copy_if_changed "$CODEX_AFT_TEMPLATE" "$CODEX_AFT_DOC"
install_templated_file "$SESSION_RUNTIME_TEMPLATE" "$CODEX_HOOKS_DIR/aft-session-runtime.sh" "$AFT_BINARY"
install_templated_file "$SESSION_START_TEMPLATE" "$CODEX_HOOKS_DIR/aft-codex-session-start.sh" "$AFT_BINARY"
install_templated_file "$STOP_TEMPLATE" "$CODEX_HOOKS_DIR/aft-codex-stop.sh" "$AFT_BINARY"
copy_if_changed "$USER_PROMPT_TEMPLATE" "$CODEX_HOOKS_DIR/aft-codex-user-prompt-submit.sh"
chmod +x \
  "$CODEX_HOOKS_DIR/aft-session-runtime.sh" \
  "$CODEX_HOOKS_DIR/aft-codex-session-start.sh" \
  "$CODEX_HOOKS_DIR/aft-codex-stop.sh" \
  "$CODEX_HOOKS_DIR/aft-codex-user-prompt-submit.sh"
info "Installed Codex hook scripts and AFT.md"

if [ -n "$GO_HELPER_BINARY" ] && [ -x "$GO_HELPER_BINARY" ]; then
  upsert_shell_helper_env "$GO_HELPER_BINARY" "$ZSH_CONFIG_FILE" "zsh"
  upsert_shell_helper_env "$GO_HELPER_BINARY" "$FISH_CONFIG_FILE" "fish"
  info "Configured AFT_GO_HELPER_PATH in $ZSH_CONFIG_FILE and $FISH_CONFIG_FILE"
fi

# Add AFT.md to the global Codex AGENTS.md include chain.
if [ -f "$CODEX_AGENTS_FILE" ]; then
  if ! grep -q "AFT\.md" "$CODEX_AGENTS_FILE"; then
    printf "\n@%s\n" "$CODEX_AFT_DOC" >> "$CODEX_AGENTS_FILE"
    info "Added @$CODEX_AFT_DOC to AGENTS.md"
  else
    info "AGENTS.md already includes an AFT reference"
  fi
else
  printf "@%s\n" "$CODEX_AFT_DOC" > "$CODEX_AGENTS_FILE"
  info "Created AGENTS.md with @$CODEX_AFT_DOC"
fi

# Update config.toml directly so the active config layer owns the hook settings.
if config_already_has_codex_settings; then
  info "config.toml already has Codex hook settings"
else
  ensure_root_boolean "suppress_unstable_features_warning" "true"
  ensure_features_boolean "codex_hooks" "true"
  info "Updated config.toml with Codex hook settings"
fi

# Update hooks.json without disturbing unrelated hooks.
if [ -f "$CODEX_HOOKS_FILE" ]; then
  TEMP_FILE="$(mktemp)"
  jq \
    --arg session_cmd "$CODEX_HOOKS_DIR/aft-codex-session-start.sh" \
    --arg stop_cmd "$CODEX_HOOKS_DIR/aft-codex-stop.sh" \
    --arg prompt_cmd "$CODEX_HOOKS_DIR/aft-codex-user-prompt-submit.sh" \
    '
      .hooks = (.hooks // {}) |
      .hooks.SessionStart = (
        ((.hooks.SessionStart // []) | map(
          . as $entry |
          (($entry.hooks // []) | map(select((.command // "") | contains("aft-codex-session-start.sh"))) | length) as $aft |
          if $aft > 0 then empty else $entry end
        )) + [
          {
            "matcher": "startup|resume",
            "hooks": [
              {
                "type": "command",
                "command": $session_cmd,
                "statusMessage": "Loading AFT guidance"
              }
            ]
          }
        ]
      ) |
      .hooks.Stop = (
        ((.hooks.Stop // []) | map(
          . as $entry |
          (($entry.hooks // []) | map(select((.command // "") | contains("aft-codex-stop.sh"))) | length) as $aft |
          if $aft > 0 then empty else $entry end
        )) + [
          {
            "hooks": [
              {
                "type": "command",
                "command": $stop_cmd,
                "timeout": 5
              }
            ]
          }
        ]
      ) |
      .hooks.UserPromptSubmit = (
        ((.hooks.UserPromptSubmit // []) | map(
          . as $entry |
          (($entry.hooks // []) | map(select((.command // "") | contains("aft-codex-user-prompt-submit.sh"))) | length) as $aft |
          if $aft > 0 then empty else $entry end
        )) + [
          {
            "hooks": [
              {
                "type": "command",
                "command": $prompt_cmd
              }
            ]
          }
        ]
      )
    ' "$CODEX_HOOKS_FILE" > "$TEMP_FILE" || error "Failed to update $CODEX_HOOKS_FILE"
  overwrite_file "$TEMP_FILE" "$CODEX_HOOKS_FILE"
  info "Refreshed AFT entries in hooks.json"
else
  TEMP_FILE="$(mktemp)"
  cat > "$TEMP_FILE" <<EOF
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume",
        "hooks": [
          {
            "type": "command",
            "command": "$CODEX_HOOKS_DIR/aft-codex-session-start.sh",
            "statusMessage": "Loading AFT guidance"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "$CODEX_HOOKS_DIR/aft-codex-stop.sh",
            "timeout": 5
          }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "$CODEX_HOOKS_DIR/aft-codex-user-prompt-submit.sh"
          }
        ]
      }
    ]
  }
}
EOF
  overwrite_file "$TEMP_FILE" "$CODEX_HOOKS_FILE"
  info "Created hooks.json with AFT hook configuration"
fi

# Add aft to PATH via symlink when possible.
if [ -d "/usr/local/bin" ] && [ -w "/usr/local/bin" ]; then
  ln -sf "$CODEX_BIN_DIR/aft" /usr/local/bin/aft 2>/dev/null && \
    info "Symlinked aft to /usr/local/bin/aft" || \
    warn "Could not symlink aft to /usr/local/bin"

  if [ -n "$GO_HELPER_BINARY" ] && [ -x "$GO_HELPER_BINARY" ]; then
    ln -sf "$GO_HELPER_BINARY" /usr/local/bin/aft-go-helper 2>/dev/null && \
      info "Symlinked aft-go-helper to /usr/local/bin/aft-go-helper" || \
      warn "Could not symlink aft-go-helper to /usr/local/bin"
  fi
else
  warn "Cannot write to /usr/local/bin - add $CODEX_BIN_DIR to PATH manually"
  if [ -n "$GO_HELPER_BINARY" ] && [ -x "$GO_HELPER_BINARY" ]; then
    warn "Also add $GO_HELPER_BINARY to PATH as aft-go-helper for Go interface dispatch resolution"
  fi
fi

echo ""
echo -e "${GREEN}AFT Codex integration installed successfully!${NC}"
echo ""
echo "Installed files:"
echo "  $CODEX_BIN_DIR/aft                         - CLI wrapper"
echo "  $CODEX_HOOKS_DIR/aft-session-runtime.sh   - Shared session lifecycle helper"
echo "  $CODEX_HOOKS_DIR/aft-codex-session-start.sh - SessionStart hook"
echo "  $CODEX_HOOKS_DIR/aft-codex-stop.sh        - Stop hook (session lease heartbeat)"
echo "  $CODEX_HOOKS_DIR/aft-codex-user-prompt-submit.sh - UserPromptSubmit hook"
echo "  $CODEX_AFT_DOC                            - Codex AFT instructions"
echo "  $CODEX_HOOKS_FILE                         - Codex hook configuration"
echo "  $CODEX_CONFIG_FILE                        - Codex feature configuration"
if [ -n "$GO_HELPER_BINARY" ] && [ -x "$GO_HELPER_BINARY" ]; then
  echo "  $GO_HELPER_BINARY                         - Go interface-dispatch resolver"
  echo "  $ZSH_CONFIG_FILE / $FISH_CONFIG_FILE      - AFT_GO_HELPER_PATH"
fi
echo ""
echo "Notes:"
echo "  Codex hooks now prewarm AFT-Go on SessionStart and refresh the lease on Stop."
echo "  They do not transparently replace Codex's non-Bash file tools."
echo "  The installed aft wrapper defaults Go overlay queries to aft_go_sidecar."
echo "  Override with AFT_GO_OVERLAY_PROVIDER or AFT_GO_OVERLAY_BACKEND if needed."
echo ""
echo "Usage:"
echo "  aft outline src/         # Get file structure"
echo "  aft zoom file.ts func    # Inspect function"
echo "  aft callers file.ts func # Find all callers"
echo ""
echo "Restart Codex to activate hooks."
