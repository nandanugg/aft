#!/usr/bin/env bash
# AFT Claude Code Hooks Installer
# Installs AFT hooks for Claude Code integration

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AFT_ROOT="$(dirname "$SCRIPT_DIR")"
CLAUDE_DIR="$HOME/.claude"
HOOKS_DIR="$CLAUDE_DIR/hooks"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info() { echo -e "${GREEN}[INFO]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

# Check for required tools
command -v jq &>/dev/null || error "jq is required but not installed. Install with: brew install jq"

# Build AFT binary if needed
AFT_BINARY="$AFT_ROOT/target/release/aft"
if [ ! -x "$AFT_BINARY" ]; then
    info "Building AFT binary..."
    cd "$AFT_ROOT"
    cargo build --release || error "Failed to build AFT binary"
fi

info "AFT binary: $AFT_BINARY"

# Build the optional Go helper for interface-dispatch resolution in Go projects.
# If `go` is not installed, we skip silently — AFT still works for all languages,
# but Go method calls won't be type-resolved (falls back to tree-sitter).
GO_HELPER_BINARY="$AFT_ROOT/target/release/aft-go-helper"
if command -v go &>/dev/null; then
    info "Building aft-go-helper (Go interface-dispatch resolver)..."
    cd "$AFT_ROOT/go-helper"
    if go build -o "$GO_HELPER_BINARY" .; then
        info "Go helper built: $GO_HELPER_BINARY"
    else
        warn "Failed to build aft-go-helper — Go interface dispatch resolution will be unavailable."
        GO_HELPER_BINARY=""
    fi
    cd "$AFT_ROOT"
else
    warn "Go toolchain not found — skipping aft-go-helper build. Install Go for type-accurate call resolution in Go projects."
    GO_HELPER_BINARY=""
fi

# Create directories
mkdir -p "$HOOKS_DIR"
info "Created hooks directory: $HOOKS_DIR"

# Write aft CLI wrapper
cat > "$HOOKS_DIR/aft" << 'WRAPPER_EOF'
#!/usr/bin/env bash
# AFT CLI wrapper for Claude Code
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
  local config_req=$(jq -cn --arg root "$work_dir" '{id:"cfg",command:"configure",project_root:$root,wait_for_helper:true}')
  local cmd_req=$(echo "$params" | jq -c --arg cmd "$cmd" '{id:"cmd",command:$cmd} + .')

  # `awk '… exit'` drains stdin safely; `grep | head -1` under `set -o pipefail`
  # triggers SIGPIPE (exit 141) on the upstream grep once the response exceeds the
  # pipe buffer, silently killing the script on large outlines.
  #
  # stderr: filter [aft] progress lines through to the user's terminal and
  # drop everything else (tree-sitter warnings, etc.). Without this the
  # first query on a large project looks like a hang — configure can take
  # 10+ seconds and there's nothing to see until the response arrives.
  local result=$( (echo "$config_req"; echo "$cmd_req") | "$AFT_BINARY" 2> >(grep --line-buffered '^\[aft\]' >&2) | awk '/"id":"cmd"/ {print; found=1; exit} END {exit !found}')

  # Check success
  local success=$(echo "$result" | jq -r '.success // false')
  if [ "$success" != "true" ]; then
    local msg=$(echo "$result" | jq -r '.message // "Command failed"')
    echo "Error: $msg" >&2
    exit 1
  fi

  # Output text or content
  local text=$(echo "$result" | jq -r '.text // .content // empty')
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
WRAPPER_EOF

# Replace placeholder with actual binary path
sed -i '' "s|__AFT_BINARY_PATH__|$AFT_BINARY|g" "$HOOKS_DIR/aft"
chmod +x "$HOOKS_DIR/aft"
info "Installed CLI wrapper: $HOOKS_DIR/aft"

# Write aft-hook.sh
cat > "$HOOKS_DIR/aft-hook.sh" << 'HOOK_EOF'
#!/usr/bin/env bash
# AFT Hook for Claude Code
# Intercepts Read, Grep, Glob tools and routes them through AFT binary

TOOL_NAME="${1:-}"
AFT_BINARY="__AFT_BINARY_PATH__"

# Check dependencies
command -v jq &>/dev/null || exit 0
[ -x "$AFT_BINARY" ] || exit 0

# Read input JSON
INPUT=$(cat)
TOOL_INPUT=$(echo "$INPUT" | jq -c '.tool_input // {}')
WORK_DIR=$(echo "$INPUT" | jq -r '.session.workingDirectory // "."')

# Call AFT binary with configure + command
call_aft() {
  local cmd="$1"
  local params="$2"

  local config_req=$(jq -cn --arg root "$WORK_DIR" '{id:"cfg",command:"configure",project_root:$root}')
  local cmd_req=$(echo "$params" | jq -c --arg cmd "$cmd" '{id:"cmd",command:$cmd} + .')

  # awk avoids the SIGPIPE-from-head-under-pipefail trap that silently killed large responses.
  (echo "$config_req"; echo "$cmd_req") | "$AFT_BINARY" 2>/dev/null | awk '/"id":"cmd"/ {print; found=1; exit} END {exit !found}'
}

case "$TOOL_NAME" in
  Read)
    FILE_PATH=$(echo "$TOOL_INPUT" | jq -r '.file_path // empty')
    [ -z "$FILE_PATH" ] && exit 0

    OFFSET=$(echo "$TOOL_INPUT" | jq -r '.offset // 0')
    LIMIT=$(echo "$TOOL_INPUT" | jq -r '.limit // 2000')
    START_LINE=$((OFFSET + 1))

    PARAMS=$(jq -cn --arg f "$FILE_PATH" --argjson s "$START_LINE" --argjson l "$LIMIT" \
      '{file:$f,start_line:$s,limit:$l}')

    RESULT=$(call_aft "read" "$PARAMS")
    [ -z "$RESULT" ] && exit 0

    SUCCESS=$(echo "$RESULT" | jq -r '.success')
    [ "$SUCCESS" != "true" ] && exit 0

    CONTENT=$(echo "$RESULT" | jq -r '.content // empty')
    [ -z "$CONTENT" ] && exit 0

    # Output to stderr for exit 2 blocking message
    echo "[AFT Read] $FILE_PATH" >&2
    echo "$CONTENT" >&2
    exit 2
    ;;

  Grep)
    PATTERN=$(echo "$TOOL_INPUT" | jq -r '.pattern // empty')
    [ -z "$PATTERN" ] && exit 0

    PATH_ARG=$(echo "$TOOL_INPUT" | jq -r '.path // "."')
    INCLUDE=$(echo "$TOOL_INPUT" | jq -r '.include // empty')

    if [ -n "$INCLUDE" ]; then
      PARAMS=$(jq -cn --arg p "$PATTERN" --arg d "$PATH_ARG" --arg i "$INCLUDE" \
        '{pattern:$p,path:$d,include:$i}')
    else
      PARAMS=$(jq -cn --arg p "$PATTERN" --arg d "$PATH_ARG" '{pattern:$p,path:$d}')
    fi

    RESULT=$(call_aft "grep" "$PARAMS")
    [ -z "$RESULT" ] && exit 0

    SUCCESS=$(echo "$RESULT" | jq -r '.success')
    [ "$SUCCESS" != "true" ] && exit 0

    CONTENT=$(echo "$RESULT" | jq -r '.text // empty')
    [ -z "$CONTENT" ] && exit 0

    echo "[AFT Grep] $PATTERN" >&2
    echo "$CONTENT" >&2
    exit 2
    ;;

  Glob)
    PATTERN=$(echo "$TOOL_INPUT" | jq -r '.pattern // empty')
    [ -z "$PATTERN" ] && exit 0

    PATH_ARG=$(echo "$TOOL_INPUT" | jq -r '.path // "."')
    PARAMS=$(jq -cn --arg p "$PATTERN" --arg d "$PATH_ARG" '{pattern:$p,path:$d}')

    RESULT=$(call_aft "glob" "$PARAMS")
    [ -z "$RESULT" ] && exit 0

    SUCCESS=$(echo "$RESULT" | jq -r '.success')
    [ "$SUCCESS" != "true" ] && exit 0

    CONTENT=$(echo "$RESULT" | jq -r '.text // empty')
    [ -z "$CONTENT" ] && exit 0

    echo "[AFT Glob] $PATTERN" >&2
    echo "$CONTENT" >&2
    exit 2
    ;;

  *)
    exit 0
    ;;
esac
HOOK_EOF

sed -i '' "s|__AFT_BINARY_PATH__|$AFT_BINARY|g" "$HOOKS_DIR/aft-hook.sh"
chmod +x "$HOOKS_DIR/aft-hook.sh"
info "Installed hook script: $HOOKS_DIR/aft-hook.sh"

# Write AFT.md instructions
cat > "$CLAUDE_DIR/AFT.md" << 'INSTRUCTIONS_EOF'
# AFT - Agent File Tools

Tree-sitter powered code analysis for massive context savings (60-90% token reduction).

## Two Kinds of Questions, Two Kinds of Tools

Every code question is either **what exists** (structure) or **what runs** (behavior). AFT has separate tools for each — pick by the question, not by caution.

**"What exists here?"** → `aft outline`. Use when you need to know which files live in a directory, which symbols a file defines, what types are declared. Outline is fast and cheap; reach for it freely when surveying a codebase.

**"How does this work / flow / connect?"** → `aft trace_to`, `aft call_tree`, `aft callers`. These are the right tool for *every* behavior or flow question, not a last resort:
- `aft call_tree <file> <symbol>` — what this function calls (forward graph).
- `aft callers <file> <symbol>` — who calls this function (reverse graph).
- `aft trace_to <file> <symbol>` — how execution reaches this point (entry-point paths).
- `aft trace_data <file> <symbol> <expr>` — how a value flows through assignments and calls.
- `aft impact <file> <symbol>` — what breaks if this changes.
- `aft zoom <file> <symbol>` — read a specific function body with call-graph annotations.

**Default to trace tools for behavior questions.** "What's the normal flow for X?", "what handles Y?", "who sends this event?", "how does the happy path work?" — all of these are trace / call_tree / callers questions. Outline-diving through directories to piece together a flow is slower and less accurate than following the call graph directly from a known entry point (HTTP handler, Kafka consumer, CLI main, etc.).

**Grep is fine for "does this string appear?"** but reach for semantic tools when the answer requires understanding the behavior behind the name.

### Performance note

First `aft` call in a project can take 10-30 seconds on a large codebase (parsing hundreds of files, running the optional Go helper). Progress lines go to stderr. Subsequent calls reuse a disk cache and are near-instant unless files changed. Cold-start slowness is not a hang — watch stderr for progress.

## AFT CLI Commands

Use `aft` commands via Bash for code navigation. These provide structured output optimized for LLM consumption.

### Semantic Commands

```bash
# Get structure without content (~10% of full read tokens)
aft outline <file|directory>

# Inspect symbol with call-graph annotations
aft zoom <file> <symbol>

# Forward call graph - what does this function call?
aft call_tree <file> <symbol>

# Reverse call graph - who calls this function?
aft callers <file> <symbol>

# Impact analysis - what breaks if this changes?
aft impact <file> <symbol>

# Control flow - how does execution reach this function?
aft trace_to <file> <symbol>

# Data flow - how does a value flow through assignments and across calls?
aft trace_data <file> <symbol> <expression> [depth]
```

### Basic Commands

```bash
aft read <file> [start_line] [limit]   # Read with line numbers
aft grep <pattern> [path]              # Trigram-indexed search
aft glob <pattern> [path]              # File pattern matching
```

## Tracing: control flow vs. data flow

Two different questions, two commands:
- **"How does execution reach this function?"** → `aft trace_to` (control flow).
  Example: `aft trace_to api/handler.go ChargePayment` — shows the call chain that lands on ChargePayment.
- **"Where did this value come from / where does it go next?"** → `aft trace_data` (data flow through assignments and parameter passing).
  Example: `aft trace_data api/handler.go ChargePayment merchantID` — traces how `merchantID` propagates within and across function boundaries.

For a bug like "this field got the wrong value," `trace_data` is usually the right starting point; for "why did this handler run," `trace_to` is.

### Patterns trace_data handles

`trace_data` follows values across these constructs — use it confidently on idiomatic code instead of manually reading every caller:

- **Direct args**: `f(x)` → hop into `f`'s matching parameter.
- **Reference args**: `f(&x)` → hop into `f`'s pointer parameter.
- **Field-access args**: `f(x.Field)` → approximate hop into `f`'s matching parameter (propagation continues).
- **Struct-literal wraps**: `w := Wrapper{Field: x}` → approximate assignment hop to `w`, then tracking continues on `w`.
- **Pointer-write intrinsics** (`json.Unmarshal`, `yaml.Unmarshal`, `xml.Unmarshal`, `toml.Unmarshal`, `proto.Unmarshal`, `bson.Unmarshal`, `msgpack.Unmarshal`): `json.Unmarshal(raw, &out)` binds `raw`'s flow into `out`, and further uses of `out` are tracked.
- **Method receivers**: `x.Method(...)` → hop into the receiver parameter name (Go `func (u *T) Method(...)`, Rust `&self`).
- **Destructuring assigns**: `a, b := f()` and `{a, b} = f()` → tracking splits onto the new bindings.

Hops marked `"approximate": true` are lossy (field access, struct wraps, writer intrinsics) — the flow exists but the exact subfield is not resolved.

## When to Use What

| Task | Command | Token Savings |
|------|---------|---------------|
| Understanding file structure | `aft outline` | ~90% vs full read |
| Finding function definition | `aft zoom file symbol` | Exact code only |
| Understanding dependencies | `aft call_tree` | Structured graph |
| Finding usage sites | `aft callers` | All call sites |
| Planning refactors | `aft impact` | Change propagation |
| Debugging control flow | `aft trace_to` | Execution paths |
| Debugging data flow | `aft trace_data` | Value propagation |

## Rules

Match the command to the question, not to caution.

**Structural / "what exists" questions** — "does X still exist?", "what files are in this dir?", "what symbols does this file declare?":
1. `aft outline` is the right tool. Fast and cheap.
2. For directory reads, always outline first to anchor; confirm which specific files are actually needed before expanding with zoom / selective reads.
3. When briefing a subagent to explore a repo, run `aft outline <path>` yourself first and include the output in the subagent prompt — subagents don't follow ordering guarantees.

**Behavioral / "what runs" questions** — "how does X work?", "what's the flow for Y?", "who calls Z?", "what happens if I change W?":
4. Reach for `aft trace_to` / `aft call_tree` / `aft callers` / `aft trace_data` / `aft impact` **first**, not as a last resort. These tools answer the question directly; outline-diving through directories to piece together a flow is slower and less accurate.
5. Start from a known entry point (HTTP handler, Kafka consumer, main, test entry) and follow the call graph out. Use `aft zoom` when you need to read the body of a specific function you've identified.
6. A zero result from `callers` or `trace_to` is itself information — but cross-check with `aft grep` on the symbol name if the result looks surprisingly sparse; some dispatch patterns (reflection, DI frameworks, callback registration) can't be resolved statically.

**`aft grep` is fine for "does this string appear?"** Reach for semantic tools when the answer requires understanding the behavior behind the name.

## Context Protection

Context is finite. Even when a user explicitly requests "contents" or "read all files":
- For directories with 5+ files, run `aft outline` first and confirm which files are actually needed.
- Never read more than 3-5 files in a single action without confirming user intent.
- "Read all files" is a request, not a command to fill context — propose outline + selective reads instead.

## Supported Languages

TypeScript, JavaScript, Python, Rust, Go, C/C++, Java, Ruby, Markdown

## Hook Integration

Read, Grep, and Glob tools are automatically routed through AFT via hooks for indexed performance.
INSTRUCTIONS_EOF

info "Installed instructions: $CLAUDE_DIR/AFT.md"

# Update CLAUDE.md to include @AFT.md
if [ -f "$CLAUDE_DIR/CLAUDE.md" ]; then
    if ! grep -q "@AFT.md" "$CLAUDE_DIR/CLAUDE.md"; then
        echo "@AFT.md" >> "$CLAUDE_DIR/CLAUDE.md"
        info "Added @AFT.md to existing CLAUDE.md"
    else
        info "CLAUDE.md already includes @AFT.md"
    fi
else
    echo "@AFT.md" > "$CLAUDE_DIR/CLAUDE.md"
    info "Created CLAUDE.md with @AFT.md"
fi

# Update settings.json with hooks
SETTINGS_FILE="$CLAUDE_DIR/settings.json"

if [ -f "$SETTINGS_FILE" ]; then
    # Check if hooks already exist
    if jq -e '.hooks.PreToolUse[] | select(.matcher == "Read") | .hooks[] | select(.command | contains("aft-hook.sh"))' "$SETTINGS_FILE" &>/dev/null; then
        info "AFT hooks already configured in settings.json"
    else
        # Add AFT hooks to existing PreToolUse array
        TEMP_FILE=$(mktemp)

        jq --arg hooks_dir "$HOOKS_DIR" '
          .hooks.PreToolUse = (
            (.hooks.PreToolUse // []) + [
              {
                "matcher": "Read",
                "hooks": [{"type": "command", "command": ($hooks_dir + "/aft-hook.sh Read")}]
              },
              {
                "matcher": "Grep",
                "hooks": [{"type": "command", "command": ($hooks_dir + "/aft-hook.sh Grep")}]
              },
              {
                "matcher": "Glob",
                "hooks": [{"type": "command", "command": ($hooks_dir + "/aft-hook.sh Glob")}]
              }
            ]
          )
        ' "$SETTINGS_FILE" > "$TEMP_FILE"

        mv "$TEMP_FILE" "$SETTINGS_FILE"
        info "Added AFT hooks to settings.json"
    fi
else
    # Create new settings.json
    cat > "$SETTINGS_FILE" << SETTINGS_EOF
{
  "\$schema": "https://json.schemastore.org/claude-code-settings.json",
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Read",
        "hooks": [{"type": "command", "command": "$HOOKS_DIR/aft-hook.sh Read"}]
      },
      {
        "matcher": "Grep",
        "hooks": [{"type": "command", "command": "$HOOKS_DIR/aft-hook.sh Grep"}]
      },
      {
        "matcher": "Glob",
        "hooks": [{"type": "command", "command": "$HOOKS_DIR/aft-hook.sh Glob"}]
      }
    ]
  }
}
SETTINGS_EOF
    info "Created settings.json with AFT hooks"
fi

# Add aft to PATH via symlink
if [ -d "/usr/local/bin" ] && [ -w "/usr/local/bin" ]; then
    ln -sf "$HOOKS_DIR/aft" /usr/local/bin/aft 2>/dev/null && \
        info "Symlinked aft to /usr/local/bin/aft" || \
        warn "Could not symlink to /usr/local/bin (run with sudo if needed)"

    # Also symlink the Go helper so find_helper_binary picks it up via PATH.
    if [ -n "$GO_HELPER_BINARY" ] && [ -x "$GO_HELPER_BINARY" ]; then
        ln -sf "$GO_HELPER_BINARY" /usr/local/bin/aft-go-helper 2>/dev/null && \
            info "Symlinked aft-go-helper to /usr/local/bin/aft-go-helper" || \
            warn "Could not symlink aft-go-helper to /usr/local/bin (run with sudo if needed)"
    fi
else
    warn "Cannot write to /usr/local/bin - add $HOOKS_DIR to PATH manually"
    if [ -n "$GO_HELPER_BINARY" ] && [ -x "$GO_HELPER_BINARY" ]; then
        warn "Also add $GO_HELPER_BINARY to PATH as 'aft-go-helper' for Go interface dispatch resolution"
    fi
fi

echo ""
echo -e "${GREEN}AFT Claude Code integration installed successfully!${NC}"
echo ""
echo "Installed files:"
echo "  $HOOKS_DIR/aft           - CLI wrapper"
echo "  $HOOKS_DIR/aft-hook.sh   - Tool interceptor"
echo "  $CLAUDE_DIR/AFT.md       - Claude instructions"
echo "  $CLAUDE_DIR/settings.json - Hook configuration"
if [ -n "$GO_HELPER_BINARY" ] && [ -x "$GO_HELPER_BINARY" ]; then
    echo "  $GO_HELPER_BINARY - Go interface-dispatch resolver"
fi
echo ""
echo "Usage:"
echo "  aft outline src/         # Get file structure"
echo "  aft zoom file.ts func    # Inspect function"
echo "  aft callers file.ts func # Find all callers"
if [ -n "$GO_HELPER_BINARY" ] && [ -x "$GO_HELPER_BINARY" ]; then
    echo ""
    echo "Go interface dispatch is enabled. AFT will automatically resolve"
    echo "interface method calls to their concrete implementations in Go projects."
fi
echo ""
echo "Restart Claude Code to activate hooks."
