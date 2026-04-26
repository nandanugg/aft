#!/usr/bin/env bash
# AFT CLI wrapper
# Usage: aft <command> [args...]
#
# Commands:
#   outline <file|dir>                       - Get file/directory structure (symbols, functions, classes)
#   zoom <file> [symbol]                     - Inspect symbol with call-graph annotations
#   call_tree <file> <symbol>                - What does this function call? (forward graph)
#   callers <file> <symbol>                  - Who calls this function? (reverse graph)
#   impact <file> <symbol>                   - What breaks if this changes?
#   trace_to <file> <symbol>                 - How does execution reach this function?
#   trace_data <file> <symbol> <expr> [depth] - How does this value flow through the code?
#   read <file> [start] [limit]              - Read file with line numbers
#   grep <pattern> [path]                    - Search with trigram index
#   glob <pattern> [path]                    - Find files by pattern

set -euo pipefail

AFT_BINARY="__AFT_BINARY_PATH__"

# Check binary exists
if [ ! -x "$AFT_BINARY" ]; then
  echo "Error: AFT binary not found at $AFT_BINARY" >&2
  exit 1
fi

# Detect a project root by walking up from the given path looking for a
# language marker (go.mod, Cargo.toml, package.json, pyproject.toml) or a
# .git directory. Without this, callers/trace_to silently scan only the
# shell's cwd — catastrophic when the target file lives in a different
# module than $PWD.
detect_project_root() {
  local start="${1:-$PWD}"
  local dir
  if [ -d "$start" ]; then
    dir="$start"
  else
    dir="$(dirname "$start")"
  fi
  # Absolutize so the walk terminates cleanly at /.
  dir="$(cd "$dir" 2>/dev/null && pwd)" || dir="$PWD"

  while [ -n "$dir" ] && [ "$dir" != "/" ]; do
    if [ -f "$dir/go.mod" ] \
        || [ -f "$dir/Cargo.toml" ] \
        || [ -f "$dir/package.json" ] \
        || [ -f "$dir/pyproject.toml" ] \
        || [ -d "$dir/.git" ]; then
      echo "$dir"
      return
    fi
    dir="$(dirname "$dir")"
  done
  echo "$PWD"
}

# Send command to AFT binary
call_aft() {
  local cmd="$1"
  local params="$2"
  local anchor="${3:-}"
  local go_overlay_provider="${AFT_GO_OVERLAY_PROVIDER:-${AFT_GO_OVERLAY_BACKEND:-aft_go_sidecar}}"

  local work_dir
  if [ -n "$anchor" ]; then
    work_dir="$(detect_project_root "$anchor")"
  else
    work_dir="$PWD"
  fi

  # wait_for_helper=true makes configure block until the Go helper
  # finishes, so same-process queries (the entire CLI flow) see resolved
  # interface-dispatch edges. Without it, the helper thread gets killed
  # when aft exits right after answering the command — cache never gets
  # written, and cross-package interface calls stay unresolved.
  local config_req
  local cmd_req
  if [ -n "$go_overlay_provider" ]; then
    config_req=$(jq -cn --arg root "$work_dir" --arg provider "$go_overlay_provider" '{id:"cfg",command:"configure",project_root:$root,go_overlay_provider:$provider,wait_for_helper:true}')
  else
    config_req=$(jq -cn --arg root "$work_dir" '{id:"cfg",command:"configure",project_root:$root,wait_for_helper:true}')
  fi
  cmd_req=$(echo "$params" | jq -c --arg cmd "$cmd" '{id:"cmd",command:$cmd} + .')

  # `awk '… exit'` drains stdin safely; `grep | head -1` under `set -o pipefail`
  # triggers SIGPIPE (exit 141) on the upstream grep once the response exceeds the
  # pipe buffer, silently killing the script on large outlines.
  #
  # stderr: filter [aft] progress lines through to the user's terminal and
  # drop everything else (tree-sitter warnings, etc.). Without this the
  # first query on a large project looks like a hang — configure can take
  # 10+ seconds and there's nothing to see until the response arrives.
  local result
  result=$( (echo "$config_req"; echo "$cmd_req") | "$AFT_BINARY" 2> >(grep --line-buffered '^\[aft\]' >&2) | awk '/"id":"cmd"/ {print; found=1; exit} END {exit !found}')

  # Check success
  local success
  success=$(echo "$result" | jq -r '.success // false')
  if [ "$success" != "true" ]; then
    local msg
    msg=$(echo "$result" | jq -r '.message // "Command failed"')
    echo "Error: $msg" >&2
    exit 1
  fi

  # Output text or content
  local text
  text=$(echo "$result" | jq -r '.text // .content // empty')
  if [ -n "$text" ]; then
    echo "$text"
  else
    echo "$result" | jq .
  fi
}

CMD="${1:-help}"
shift || true

case "$CMD" in
  outline)
    FILE="${1:-}"
    [ -z "$FILE" ] && { echo "Usage: aft outline <file|directory>"; exit 1; }

    # Check if directory - discover source files
    if [ -d "$FILE" ]; then
      # `awk 'NR<=100'` caps output without SIGPIPE-ing the upstream find;
      # `head -100` would close stdin early and, under `set -o pipefail`, kill the script.
      FILES=$(find "$FILE" -type f \( -name "*.ts" -o -name "*.tsx" -o -name "*.js" -o -name "*.jsx" \
        -o -name "*.py" -o -name "*.rs" -o -name "*.go" -o -name "*.c" -o -name "*.cpp" -o -name "*.h" \
        -o -name "*.java" -o -name "*.rb" -o -name "*.md" \) \
        ! -path "*/node_modules/*" ! -path "*/.git/*" ! -path "*/target/*" ! -path "*/dist/*" \
        2>/dev/null | awk 'NR<=100' | jq -R . | jq -s .)

      FILE_COUNT=$(echo "$FILES" | jq 'length')
      if [ "$FILE_COUNT" = "0" ]; then
        echo "No supported source files found in '$FILE' (looked for .ts/.tsx/.js/.jsx/.py/.rs/.go/.c/.cpp/.h/.java/.rb/.md, excluding node_modules/target/dist/.git)." >&2
        exit 1
      fi
      PARAMS=$(jq -cn --argjson files "$FILES" '{files:$files}')
    else
      PARAMS=$(jq -cn --arg f "$FILE" '{file:$f}')
    fi
    call_aft "outline" "$PARAMS" "$FILE"
    ;;

  zoom)
    FILE="${1:-}"
    SYMBOL="${2:-}"
    [ -z "$FILE" ] && { echo "Usage: aft zoom <file> [symbol]"; exit 1; }

    if [ -n "$SYMBOL" ]; then
      PARAMS=$(jq -cn --arg f "$FILE" --arg s "$SYMBOL" '{file:$f,symbol:$s}')
    else
      PARAMS=$(jq -cn --arg f "$FILE" '{file:$f}')
    fi
    call_aft "zoom" "$PARAMS" "$FILE"
    ;;

  call_tree)
    FILE="${1:-}"
    SYMBOL="${2:-}"
    [ -z "$FILE" ] || [ -z "$SYMBOL" ] && { echo "Usage: aft call_tree <file> <symbol>"; exit 1; }

    PARAMS=$(jq -cn --arg f "$FILE" --arg s "$SYMBOL" '{file:$f,symbol:$s}')
    call_aft "call_tree" "$PARAMS" "$FILE"
    ;;

  callers)
    FILE="${1:-}"
    SYMBOL="${2:-}"
    [ -z "$FILE" ] || [ -z "$SYMBOL" ] && { echo "Usage: aft callers <file> <symbol>"; exit 1; }

    PARAMS=$(jq -cn --arg f "$FILE" --arg s "$SYMBOL" '{file:$f,symbol:$s}')
    call_aft "callers" "$PARAMS" "$FILE"
    ;;

  impact)
    FILE="${1:-}"
    SYMBOL="${2:-}"
    [ -z "$FILE" ] || [ -z "$SYMBOL" ] && { echo "Usage: aft impact <file> <symbol>"; exit 1; }

    PARAMS=$(jq -cn --arg f "$FILE" --arg s "$SYMBOL" '{file:$f,symbol:$s}')
    call_aft "impact" "$PARAMS" "$FILE"
    ;;

  trace_to)
    FILE="${1:-}"
    SYMBOL="${2:-}"
    [ -z "$FILE" ] || [ -z "$SYMBOL" ] && { echo "Usage: aft trace_to <file> <symbol>"; exit 1; }

    PARAMS=$(jq -cn --arg f "$FILE" --arg s "$SYMBOL" '{file:$f,symbol:$s}')
    call_aft "trace_to" "$PARAMS" "$FILE"
    ;;

  trace_data)
    FILE="${1:-}"
    SYMBOL="${2:-}"
    EXPR="${3:-}"
    DEPTH="${4:-5}"
    if [ -z "$FILE" ] || [ -z "$SYMBOL" ] || [ -z "$EXPR" ]; then
      echo "Usage: aft trace_data <file> <symbol> <expression> [depth]"
      echo "  Traces how <expression> flows through assignments and across function boundaries."
      echo "  <symbol> is the function containing the expression; [depth] defaults to 5 (max 100)."
      exit 1
    fi

    PARAMS=$(jq -cn --arg f "$FILE" --arg s "$SYMBOL" --arg e "$EXPR" --argjson d "$DEPTH" \
      '{file:$f,symbol:$s,expression:$e,depth:$d}')
    call_aft "trace_data" "$PARAMS" "$FILE"
    ;;

  read)
    FILE="${1:-}"
    START="${2:-1}"
    LIMIT="${3:-2000}"
    [ -z "$FILE" ] && { echo "Usage: aft read <file> [start_line] [limit]"; exit 1; }

    PARAMS=$(jq -cn --arg f "$FILE" --argjson s "$START" --argjson l "$LIMIT" \
      '{file:$f,start_line:$s,limit:$l}')
    call_aft "read" "$PARAMS" "$FILE"
    ;;

  grep)
    PATTERN="${1:-}"
    PATH_ARG="${2:-.}"
    [ -z "$PATTERN" ] && { echo "Usage: aft grep <pattern> [path]"; exit 1; }

    PARAMS=$(jq -cn --arg p "$PATTERN" --arg d "$PATH_ARG" '{pattern:$p,path:$d}')
    call_aft "grep" "$PARAMS"
    ;;

  glob)
    PATTERN="${1:-}"
    PATH_ARG="${2:-.}"
    [ -z "$PATTERN" ] && { echo "Usage: aft glob <pattern> [path]"; exit 1; }

    PARAMS=$(jq -cn --arg p "$PATTERN" --arg d "$PATH_ARG" '{pattern:$p,path:$d}')
    call_aft "glob" "$PARAMS"
    ;;

  help|--help|-h)
    cat << 'EOF'
AFT - Agent File Tools (Tree-sitter powered code analysis)

SEMANTIC COMMANDS (massive context savings):
  aft outline <file|dir>           Structure without content (~10% tokens)
  aft zoom <file> <symbol>         Symbol + call graph annotations
  aft call_tree <file> <symbol>    Forward call graph (what does it call?)
  aft callers <file> <symbol>      Reverse call graph (who calls it?)
  aft impact <file> <symbol>       What breaks if this changes?
  aft trace_to <file> <symbol>     How does execution reach this?
  aft trace_data <file> <symbol> <expr> [depth]
                                   How does a value flow through assignments/calls?

BASIC COMMANDS:
  aft read <file> [start] [limit]  Read with line numbers
  aft grep <pattern> [path]        Trigram-indexed search
  aft glob <pattern> [path]        File pattern matching

EXAMPLES:
  aft outline src/                     # Get structure of all files in src/
  aft zoom main.go main                # Inspect main() with call graph
  aft callers api.go HandleRequest     # Find all callers
  aft call_tree service.go Process     # See what Process() calls
  aft trace_data svc.go handle userId  # Trace where userId came from and where it goes
EOF
    ;;

  *)
    echo "Unknown command: $CMD"
    echo "Run 'aft help' for usage"
    exit 1
    ;;
esac
